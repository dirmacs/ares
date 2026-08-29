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

#[derive(Debug, Deserialize)]
pub struct CreateTemplateRequest {
    pub product_type: String,
    pub agent_name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub config: serde_json::Value,
}

// =============================================================================
// Pure helpers (resolution, validation, row mapping)
// =============================================================================

const DEFAULT_MAX_TOOL_ITERATIONS: usize = 5;

/// Parsed tenant-agent JSONB config used for resolution and CRUD validation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantAgentConfig {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default = "default_max_tool_iterations_json")]
    pub max_tool_iterations: usize,
    #[serde(default)]
    pub parallel_tools: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub sandbox: bool,
}

fn default_max_tool_iterations_json() -> usize {
    DEFAULT_MAX_TOOL_ITERATIONS
}

/// Subset of `tenant_agents` row fields used by resolution helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantAgentRowSnapshot {
    pub enabled: bool,
    pub config: serde_json::Value,
    pub updated_at: i64,
}

/// Outcome of pure tenant-agent config resolution (registry fallback vs tenant DB).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TenantAgentResolveOutcome {
    UseTenantDb {
        config: TenantAgentConfig,
        config_version: String,
    },
    UseRegistryFallback,
}

/// Decomposed row values for `agent_from_row` (testable without a live `PgRow`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantAgentRowData {
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

pub fn tenant_agent_disabled_error(agent_name: &str, tenant_id: &str) -> AppError {
    AppError::NotFound(format!(
        "Agent '{}' is disabled for tenant '{}'",
        agent_name, tenant_id
    ))
}

pub fn tenant_agent_not_found_error(agent_name: &str, tenant_id: &str) -> AppError {
    AppError::NotFound(format!(
        "Agent '{}' not found for tenant '{}'",
        agent_name, tenant_id
    ))
}

pub fn validate_tenant_config(value: &serde_json::Value) -> Result<TenantAgentConfig> {
    let obj = value.as_object().ok_or_else(|| {
        AppError::InvalidInput("Tenant agent config must be a JSON object".into())
    })?;

    let model = obj
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::InvalidInput(
                "Tenant agent config is missing a valid non-empty 'model'".into(),
            )
        })?
        .to_string();

    let system_prompt = match obj.get("system_prompt") {
        Some(serde_json::Value::Null) | None => None,
        Some(serde_json::Value::String(value)) => Some(value.clone()),
        Some(_) => {
            return Err(AppError::InvalidInput(
                "Tenant agent config field 'system_prompt' must be a string".into(),
            ));
        }
    };

    let tools = match obj.get("tools") {
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .map(|value| {
                value.as_str().map(|s| s.to_string()).ok_or_else(|| {
                    AppError::InvalidInput(
                        "Tenant agent config field 'tools' must be an array of strings".into(),
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?,
        Some(serde_json::Value::Null) | None => Vec::new(),
        Some(_) => {
            return Err(AppError::InvalidInput(
                "Tenant agent config field 'tools' must be an array".into(),
            ));
        }
    };

    let max_tool_iterations = match obj.get("max_tool_iterations") {
        Some(serde_json::Value::Number(value)) => value.as_u64().ok_or_else(|| {
            AppError::InvalidInput(
                "Tenant agent config field 'max_tool_iterations' must be a non-negative integer"
                    .into(),
            )
        })? as usize,
        Some(serde_json::Value::Null) | None => DEFAULT_MAX_TOOL_ITERATIONS,
        Some(_) => {
            return Err(AppError::InvalidInput(
                "Tenant agent config field 'max_tool_iterations' must be a number".into(),
            ));
        }
    };

    let parallel_tools = match obj.get("parallel_tools") {
        Some(serde_json::Value::Bool(value)) => *value,
        Some(serde_json::Value::Null) | None => false,
        Some(_) => {
            return Err(AppError::InvalidInput(
                "Tenant agent config field 'parallel_tools' must be a boolean".into(),
            ));
        }
    };

    let version = match obj.get("version") {
        Some(serde_json::Value::Null) | None => None,
        Some(serde_json::Value::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Some(_) => {
            return Err(AppError::InvalidInput(
                "Tenant agent config field 'version' must be a string".into(),
            ));
        }
    };

    let sandbox = match obj.get("sandbox") {
        Some(serde_json::Value::Bool(value)) => *value,
        Some(serde_json::Value::Null) | None => false,
        Some(_) => {
            return Err(AppError::InvalidInput(
                "Tenant agent config field 'sandbox' must be a boolean".into(),
            ));
        }
    };

    Ok(TenantAgentConfig {
        model,
        system_prompt,
        tools,
        max_tool_iterations,
        parallel_tools,
        version,
        sandbox,
    })
}

pub fn tenant_config_version(config: &serde_json::Value, updated_at: i64) -> String {
    config
        .get("version")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .unwrap_or_else(|| format!("tenant-db:{}", updated_at))
}

pub fn resolve_agent_config(
    agent_name: &str,
    tenant_id: &str,
    snapshot: Option<TenantAgentRowSnapshot>,
) -> Result<TenantAgentResolveOutcome> {
    let Some(snapshot) = snapshot else {
        return Ok(TenantAgentResolveOutcome::UseRegistryFallback);
    };
    if !snapshot.enabled {
        return Err(tenant_agent_disabled_error(agent_name, tenant_id));
    }
    let config = validate_tenant_config(&snapshot.config)?;
    Ok(TenantAgentResolveOutcome::UseTenantDb {
        config_version: tenant_config_version(&snapshot.config, snapshot.updated_at),
        config,
    })
}

pub fn resolve_required_agent_config(
    agent_name: &str,
    tenant_id: &str,
    snapshot: Option<TenantAgentRowSnapshot>,
) -> Result<TenantAgentResolveOutcome> {
    let Some(snapshot) = snapshot else {
        return Err(tenant_agent_not_found_error(agent_name, tenant_id));
    };
    resolve_agent_config(agent_name, tenant_id, Some(snapshot))
}

pub fn build_tenant_agent(
    id: String,
    tenant_id: &str,
    req: &CreateTenantAgentRequest,
    now: i64,
) -> TenantAgent {
    build_tenant_agent_with_enabled(id, tenant_id, req, now, true)
}

pub fn build_tenant_agent_with_enabled(
    id: String,
    tenant_id: &str,
    req: &CreateTenantAgentRequest,
    now: i64,
    enabled: bool,
) -> TenantAgent {
    TenantAgent {
        id,
        tenant_id: tenant_id.to_string(),
        agent_name: req.agent_name.clone(),
        display_name: req.display_name.clone(),
        description: req.description.clone(),
        config: req.config.clone(),
        enabled,
        created_at: now,
        updated_at: now,
    }
}

pub fn prepare_create_tenant_agent(req: &CreateTenantAgentRequest) -> Result<Vec<String>> {
    Ok(validate_tenant_config(&req.config)?.tools)
}

pub fn merge_tenant_agent_update(
    current: &TenantAgent,
    req: &UpdateTenantAgentRequest,
    now: i64,
) -> TenantAgent {
    TenantAgent {
        display_name: req
            .display_name
            .clone()
            .unwrap_or_else(|| current.display_name.clone()),
        description: req
            .description
            .clone()
            .or_else(|| current.description.clone()),
        config: req.config.clone().unwrap_or_else(|| current.config.clone()),
        enabled: req.enabled.unwrap_or(current.enabled),
        updated_at: now,
        ..current.clone()
    }
}

pub fn agent_from_row_data(data: TenantAgentRowData) -> TenantAgent {
    TenantAgent {
        id: data.id,
        tenant_id: data.tenant_id,
        agent_name: data.agent_name,
        display_name: data.display_name,
        description: data.description,
        config: data.config,
        enabled: data.enabled,
        created_at: data.created_at,
        updated_at: data.updated_at,
    }
}

pub(crate) fn agent_from_row(row: &sqlx::postgres::PgRow) -> TenantAgent {
    agent_from_row_data(TenantAgentRowData {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        agent_name: row.get("agent_name"),
        display_name: row.get("display_name"),
        description: row.get("description"),
        config: row.get::<serde_json::Value, _>("config"),
        enabled: row.get("enabled"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
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

    let restored = agent_from_row(&row);
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

    Ok(rows.iter().map(agent_from_row).collect())
}

pub async fn list_all_tenant_agents(
    pool: &PgPool,
    tenant_id: Option<&str>,
) -> Result<Vec<TenantAgent>> {
    let rows = if let Some(tid) = tenant_id {
        sqlx::query(
            "SELECT id, tenant_id, agent_name, display_name, description, config, enabled, created_at, updated_at
             FROM tenant_agents WHERE tenant_id = $1 ORDER BY tenant_id, agent_name"
        )
        .bind(tid)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?
    } else {
        sqlx::query(
            "SELECT id, tenant_id, agent_name, display_name, description, config, enabled, created_at, updated_at
             FROM tenant_agents ORDER BY tenant_id, agent_name"
        )
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?
    };

    Ok(rows.iter().map(agent_from_row).collect())
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

    Ok(agent_from_row(&row))
}

pub async fn create_tenant_agent(
    pool: &PgPool,
    tenant_id: &str,
    req: CreateTenantAgentRequest,
) -> Result<TenantAgent> {
    prepare_create_tenant_agent(&req)?;
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

    let merged = merge_tenant_agent_update(&current, &req, now);
    if let Some(config) = req.config.as_ref() {
        validate_tenant_config(config)?;
    }

    sqlx::query(
        "UPDATE tenant_agents SET display_name = $1, description = $2, config = $3, enabled = $4, updated_at = $5
         WHERE tenant_id = $6 AND agent_name = $7"
    )
    .bind(&merged.display_name)
    .bind(&merged.description)
    .bind(&merged.config)
    .bind(merged.enabled)
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

// =============================================================================
// Template Store
// =============================================================================

pub struct AgentTemplateStore {
    pool: PgPool,
}

impl AgentTemplateStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn list_templates(&self) -> Result<Vec<AgentTemplate>> {
        list_agent_templates(&self.pool, None).await
    }

    pub async fn get_template(&self, id: &str) -> Result<Option<AgentTemplate>> {
        let row = sqlx::query(
            "SELECT id, product_type, agent_name, display_name, description, config, created_at
             FROM agent_templates WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(row.map(|row| AgentTemplate {
            id: row.get("id"),
            product_type: row.get("product_type"),
            agent_name: row.get("agent_name"),
            display_name: row.get("display_name"),
            description: row.get("description"),
            config: row.get::<serde_json::Value, _>("config"),
            created_at: row.get("created_at"),
        }))
    }

    pub async fn create_template(&self, req: &CreateTemplateRequest) -> Result<AgentTemplate> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_ts();

        sqlx::query(
            "INSERT INTO agent_templates (id, product_type, agent_name, display_name, description, config, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)"
        )
        .bind(&id)
        .bind(&req.product_type)
        .bind(&req.agent_name)
        .bind(&req.display_name)
        .bind(&req.description)
        .bind(&req.config)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(AgentTemplate {
            id,
            product_type: req.product_type.clone(),
            agent_name: req.agent_name.clone(),
            display_name: req.display_name.clone(),
            description: req.description.clone(),
            config: req.config.clone(),
            created_at: now,
        })
    }

    pub async fn delete_template(&self, id: &str) -> Result<u64> {
        let result = sqlx::query("DELETE FROM agent_templates WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(result.rows_affected())
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
        // Pre-built fleet templates
        TemplateSpec {
            product_type: "fleet",
            agent_name: "customer_support",
            display_name: "Customer Support Agent",
            description: "Friendly support agent with access to ticket and knowledge-base tools",
            model: "fast",
            system_prompt: "You are a customer support specialist. Be empathetic, clear, and concise. Use the available tools to look up tickets, search the knowledge base, and escalate when needed. Always confirm resolution before closing.",
        },
        TemplateSpec {
            product_type: "fleet",
            agent_name: "document_extraction",
            display_name: "Document Extraction Agent",
            description: "Extracts structured data from documents using extraction tools",
            model: "fast",
            system_prompt: "You are a document extraction specialist. Analyze uploaded documents and extract structured data into the requested schema. Use extraction tools for tables, forms, and named entities. Return clean JSON when structured output is requested.",
        },
        TemplateSpec {
            product_type: "fleet",
            agent_name: "research",
            display_name: "Research Agent",
            description: "Deep analysis agent with search and web scrape tools",
            model: "smart",
            system_prompt: "You are a research analyst. Break down complex questions into sub-queries, use search and web_scrape tools to gather evidence, and synthesize findings with citations. Always state confidence levels and flag uncertain claims.",
        },
        TemplateSpec {
            product_type: "fleet",
            agent_name: "data_entry",
            display_name: "Data Entry Agent",
            description: "Validates and submits form data with validation tools",
            model: "fast",
            system_prompt: "You are a data entry assistant. Validate inputs using the available validation tools before submitting. Flag any missing required fields, format errors, or data inconsistencies. Confirm each successful submission.",
        },
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
        assert_eq!(tenant_agent_version_key("", "agent"), "tenant::agent");
    }

    #[test]
    fn tenant_agent_version_key_empty_agent() {
        assert_eq!(tenant_agent_version_key("tenant", ""), "tenant:tenant:");
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
        assert_eq!(active_runtime_config_version(&agent), "tenant-db:20");
    }

    #[test]
    fn active_runtime_config_version_empty_string_falls_back() {
        let mut agent = sample_agent();
        agent.config = serde_json::json!({ "version": "" });
        assert_eq!(active_runtime_config_version(&agent), "tenant-db:20");
    }

    #[test]
    fn active_runtime_config_version_whitespace_only_falls_back() {
        let mut agent = sample_agent();
        agent.config = serde_json::json!({ "version": "   " });
        assert_eq!(active_runtime_config_version(&agent), "tenant-db:20");
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
        assert_eq!(
            suffix.len(),
            36,
            "UUID should be 36 chars, got '{}'",
            suffix
        );
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
        assert_eq!(
            snapshot
                .get("tenant_agent")
                .and_then(|v| v.get("agent_name")),
            Some(&serde_json::json!("listener"))
        );

        let restored = tenant_agent_from_snapshot(snapshot).expect("should deserialize nested");
        assert_eq!(restored.agent_name, "listener");
    }

    #[test]
    fn tenant_agent_from_snapshot_without_nested_key() {
        let agent = sample_agent();
        let raw = serde_json::to_value(&agent).unwrap();
        let restored = tenant_agent_from_snapshot(raw).expect("should deserialize flat agent");
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
        assert_eq!(snap["runtime_config_version"].as_str(), Some("v2.1.0"));
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

    // =====================================================================
    // TenantAgentConfig + pure resolution / create helpers (R39)
    // =====================================================================

    fn valid_config_json() -> serde_json::Value {
        serde_json::json!({
            "model": "fast",
            "system_prompt": "hello",
            "tools": ["search", "read"],
            "max_tool_iterations": 7,
            "parallel_tools": true,
            "version": "v1"
        })
    }

    fn row_snapshot(
        enabled: bool,
        config: serde_json::Value,
        updated_at: i64,
    ) -> TenantAgentRowSnapshot {
        TenantAgentRowSnapshot {
            enabled,
            config,
            updated_at,
        }
    }

    #[test]
    fn tenant_agent_config_serde_roundtrip() {
        let cfg = validate_tenant_config(&valid_config_json()).expect("valid");
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: TenantAgentConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, cfg);
    }

    #[test]
    fn tenant_agent_config_defaults_tools_and_iterations() {
        let cfg = validate_tenant_config(&serde_json::json!({"model": "m"})).expect("valid");
        assert!(cfg.tools.is_empty());
        assert_eq!(cfg.max_tool_iterations, 5);
        assert!(!cfg.parallel_tools);
    }

    #[test]
    fn validate_tenant_config_rejects_non_object() {
        assert!(validate_tenant_config(&serde_json::json!([])).is_err());
    }

    #[test]
    fn validate_tenant_config_rejects_missing_model() {
        assert!(validate_tenant_config(&serde_json::json!({})).is_err());
    }

    #[test]
    fn validate_tenant_config_rejects_empty_model() {
        assert!(validate_tenant_config(&serde_json::json!({"model": "  "})).is_err());
    }

    #[test]
    fn validate_tenant_config_rejects_non_string_tools_entries() {
        assert!(validate_tenant_config(&serde_json::json!({"model": "m", "tools": [1]})).is_err());
    }

    #[test]
    fn validate_tenant_config_rejects_non_array_tools() {
        assert!(
            validate_tenant_config(&serde_json::json!({"model": "m", "tools": "search"})).is_err()
        );
    }

    #[test]
    fn validate_tenant_config_rejects_negative_max_tool_iterations() {
        assert!(validate_tenant_config(
            &serde_json::json!({"model": "m", "max_tool_iterations": -1})
        )
        .is_err());
    }

    #[test]
    fn validate_tenant_config_rejects_non_boolean_parallel_tools() {
        assert!(validate_tenant_config(
            &serde_json::json!({"model": "m", "parallel_tools": "yes"})
        )
        .is_err());
    }

    #[test]
    fn validate_tenant_config_accepts_null_system_prompt() {
        let cfg = validate_tenant_config(&serde_json::json!({"model": "m", "system_prompt": null}))
            .expect("valid");
        assert!(cfg.system_prompt.is_none());
    }

    #[test]
    fn validate_tenant_config_strips_empty_version() {
        let cfg = validate_tenant_config(&serde_json::json!({"model": "m", "version": "  "}))
            .expect("valid");
        assert!(cfg.version.is_none());
    }

    #[test]
    fn tenant_config_version_prefers_config_version_field() {
        assert_eq!(
            tenant_config_version(&serde_json::json!({"version": "v9"}), 42),
            "v9"
        );
    }

    #[test]
    fn tenant_config_version_falls_back_to_updated_at() {
        assert_eq!(
            tenant_config_version(&serde_json::json!({"model": "m"}), 99),
            "tenant-db:99"
        );
    }

    #[test]
    fn resolve_agent_config_uses_registry_when_row_missing() {
        let outcome = resolve_agent_config("a", "t", None).expect("ok");
        assert_eq!(outcome, TenantAgentResolveOutcome::UseRegistryFallback);
    }

    #[test]
    fn resolve_agent_config_returns_tenant_db_when_enabled() {
        let outcome = resolve_agent_config(
            "listener",
            "tenant-1",
            Some(row_snapshot(true, valid_config_json(), 55)),
        )
        .expect("ok");
        match outcome {
            TenantAgentResolveOutcome::UseTenantDb {
                config: parsed,
                config_version,
            } => {
                assert_eq!(parsed.tools, vec!["search".to_string(), "read".to_string()]);
                assert_eq!(config_version, "v1");
            }
            TenantAgentResolveOutcome::UseRegistryFallback => panic!("expected tenant db"),
        }
    }

    #[test]
    fn resolve_agent_config_errors_when_disabled() {
        let err = resolve_agent_config(
            "listener",
            "tenant-1",
            Some(row_snapshot(false, valid_config_json(), 1)),
        )
        .unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[test]
    fn resolve_agent_config_errors_on_invalid_config() {
        let err = resolve_agent_config(
            "listener",
            "tenant-1",
            Some(row_snapshot(true, serde_json::json!({"tools": []}), 1)),
        )
        .unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }

    #[test]
    fn resolve_required_agent_config_errors_when_row_missing() {
        assert!(matches!(
            resolve_required_agent_config("a", "t", None).unwrap_err(),
            AppError::NotFound(_)
        ));
    }

    #[test]
    fn resolve_required_agent_config_succeeds_when_present() {
        let outcome = resolve_required_agent_config(
            "a",
            "t",
            Some(row_snapshot(true, serde_json::json!({"model": "m"}), 3)),
        )
        .expect("ok");
        assert!(matches!(
            outcome,
            TenantAgentResolveOutcome::UseTenantDb { .. }
        ));
    }

    #[test]
    fn build_tenant_agent_maps_request_fields() {
        let req = CreateTenantAgentRequest {
            agent_name: "bot".into(),
            display_name: "Bot".into(),
            description: Some("d".into()),
            config: serde_json::json!({"model": "fast"}),
        };
        let agent = build_tenant_agent("id-1".into(), "tenant-9", &req, 123);
        assert_eq!(agent.id, "id-1");
        assert_eq!(agent.tenant_id, "tenant-9");
        assert!(agent.enabled);
    }

    #[test]
    fn build_tenant_agent_with_enabled_respects_flag() {
        let req = CreateTenantAgentRequest {
            agent_name: "bot".into(),
            display_name: "Bot".into(),
            description: None,
            config: serde_json::json!({"model": "fast"}),
        };
        let agent = build_tenant_agent_with_enabled("id".into(), "t", &req, 1, false);
        assert!(!agent.enabled);
    }

    #[test]
    fn prepare_create_tenant_agent_returns_selected_tools() {
        let req = CreateTenantAgentRequest {
            agent_name: "bot".into(),
            display_name: "Bot".into(),
            description: None,
            config: valid_config_json(),
        };
        assert_eq!(
            prepare_create_tenant_agent(&req).expect("tools"),
            vec!["search".to_string(), "read".to_string()]
        );
    }

    #[test]
    fn prepare_create_tenant_agent_errors_on_invalid_config() {
        let req = CreateTenantAgentRequest {
            agent_name: "bot".into(),
            display_name: "Bot".into(),
            description: None,
            config: serde_json::json!({}),
        };
        assert!(prepare_create_tenant_agent(&req).is_err());
    }

    #[test]
    fn merge_tenant_agent_update_preserves_unset_fields() {
        let current = sample_agent();
        let req = UpdateTenantAgentRequest {
            display_name: Some("New".into()),
            description: None,
            config: None,
            enabled: None,
        };
        let merged = merge_tenant_agent_update(&current, &req, 999);
        assert_eq!(merged.display_name, "New");
        assert_eq!(merged.description, current.description);
    }

    #[test]
    fn merge_tenant_agent_update_applies_config_and_enabled() {
        let current = sample_agent();
        let req = UpdateTenantAgentRequest {
            display_name: None,
            description: Some("x".into()),
            config: Some(serde_json::json!({"model": "slow"})),
            enabled: Some(false),
        };
        let merged = merge_tenant_agent_update(&current, &req, 5);
        assert!(!merged.enabled);
        assert_eq!(merged.config["model"], "slow");
    }

    #[test]
    fn agent_from_row_data_maps_all_columns() {
        let data = TenantAgentRowData {
            id: "id".into(),
            tenant_id: "tenant".into(),
            agent_name: "agent".into(),
            display_name: "Display".into(),
            description: None,
            config: serde_json::json!({"model": "m"}),
            enabled: false,
            created_at: 1,
            updated_at: 2,
        };
        let agent = agent_from_row_data(data.clone());
        assert_eq!(agent.id, data.id);
        assert!(!agent.enabled);
    }

    #[test]
    fn agent_from_row_data_preserves_optional_description() {
        let agent = agent_from_row_data(TenantAgentRowData {
            id: "id".into(),
            tenant_id: "t".into(),
            agent_name: "a".into(),
            display_name: "d".into(),
            description: Some("desc".into()),
            config: serde_json::json!({}),
            enabled: true,
            created_at: 0,
            updated_at: 0,
        });
        assert_eq!(agent.description.as_deref(), Some("desc"));
    }

    #[test]
    fn tenant_agent_disabled_error_message_contains_ids() {
        let msg = tenant_agent_disabled_error("bot", "tenant-x").to_string();
        assert!(msg.contains("bot") && msg.contains("tenant-x"));
    }

    #[test]
    fn tenant_agent_not_found_error_message_contains_ids() {
        let msg = tenant_agent_not_found_error("bot", "tenant-x").to_string();
        assert!(msg.contains("bot") && msg.contains("tenant-x"));
    }

    #[test]
    fn tenant_agent_config_debug_includes_model() {
        let cfg = validate_tenant_config(&serde_json::json!({"model": "fast"})).unwrap();
        assert!(format!("{:?}", cfg).contains("fast"));
    }

    #[test]
    fn tenant_agent_row_snapshot_equality() {
        let a = row_snapshot(true, serde_json::json!({"model": "m"}), 1);
        assert_eq!(a, row_snapshot(true, serde_json::json!({"model": "m"}), 1));
    }

    #[test]
    fn insert_tenant_agent_sql_constant_matches_create_bindings() {
        assert!(INSERT_TENANT_AGENT_SQL.contains("$7, $7"));
    }

    #[test]
    fn delete_tenant_agent_sql_targets_composite_key() {
        assert!(DELETE_TENANT_AGENT_SQL.contains("tenant_id = $1"));
        assert!(DELETE_TENANT_AGENT_SQL.contains("agent_name = $2"));
    }

    #[test]
    fn resolve_agent_config_tool_selection_exposed_in_outcome() {
        let outcome = resolve_agent_config(
            "x",
            "t",
            Some(row_snapshot(
                true,
                serde_json::json!({"model": "m", "tools": ["a", "b", "c"]}),
                1,
            )),
        )
        .expect("ok");
        if let TenantAgentResolveOutcome::UseTenantDb { config, .. } = outcome {
            assert_eq!(config.tools.len(), 3);
        } else {
            panic!("expected tenant db");
        }
    }

    #[cfg(feature = "postgres")]
    mod postgres_integration {
        use super::*;
        use sqlx::PgPool;

        async fn try_test_pool() -> PgPool {
            ares_test_support::pool().await
        }

        fn unique_tenant() -> String {
            format!("tenant-test-{}", uuid::Uuid::new_v4())
        }

        #[tokio::test]
        async fn integration_create_get_delete_tenant_agent() {
            let pool = try_test_pool().await;
            let tenant_id = unique_tenant();
            sqlx::query("INSERT INTO tenants (id, name, tier, created_at, updated_at) VALUES ($1, $2, 'free', 1, 1) ON CONFLICT (id) DO NOTHING")
                .bind(&tenant_id).bind("Test Tenant").execute(&pool).await.expect("tenant");
            let created = create_tenant_agent(
                &pool,
                &tenant_id,
                CreateTenantAgentRequest {
                    agent_name: "product".into(),
                    display_name: "Product".into(),
                    description: Some("i".into()),
                    config: serde_json::json!({"model": "fast", "tools": ["search"]}),
                },
            )
            .await
            .expect("create");
            assert_eq!(created.agent_name, "product");
            let fetched = get_tenant_agent(&pool, &tenant_id, "product")
                .await
                .expect("get");
            assert_eq!(fetched.id, created.id);
            delete_tenant_agent(&pool, &tenant_id, "product")
                .await
                .expect("delete");
            assert!(get_tenant_agent(&pool, &tenant_id, "product")
                .await
                .is_err());
        }

        #[tokio::test]
        async fn integration_create_rejects_invalid_config() {
            let pool = try_test_pool().await;
            let err = create_tenant_agent(
                &pool,
                &unique_tenant(),
                CreateTenantAgentRequest {
                    agent_name: "broken".into(),
                    display_name: "Broken".into(),
                    description: None,
                    config: serde_json::json!({"tools": []}),
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(err, AppError::InvalidInput(_)));
        }

        #[tokio::test]
        async fn integration_list_tenant_agents_orders_by_name() {
            let pool = try_test_pool().await;
            let tenant_id = unique_tenant();
            sqlx::query("INSERT INTO tenants (id, name, tier, created_at, updated_at) VALUES ($1, $2, 'free', 1, 1) ON CONFLICT (id) DO NOTHING")
                .bind(&tenant_id).bind("List Tenant").execute(&pool).await.expect("tenant");
            for name in ["zebra", "alpha"] {
                create_tenant_agent(
                    &pool,
                    &tenant_id,
                    CreateTenantAgentRequest {
                        agent_name: name.into(),
                        display_name: name.into(),
                        description: None,
                        config: serde_json::json!({"model": "fast"}),
                    },
                )
                .await
                .expect("create");
            }
            let names: Vec<_> = list_tenant_agents(&pool, &tenant_id)
                .await
                .expect("list")
                .into_iter()
                .map(|a| a.agent_name)
                .collect();
            assert_eq!(names, vec!["alpha".to_string(), "zebra".to_string()]);
        }
    }
    #[test]
    fn validate_tenant_config_accepts_parallel_tools_true() {
        let cfg =
            validate_tenant_config(&serde_json::json!({"model": "m", "parallel_tools": true}))
                .unwrap();
        assert!(cfg.parallel_tools);
    }

    #[test]
    fn build_tenant_agent_copies_config_object() {
        let req = CreateTenantAgentRequest {
            agent_name: "a".into(),
            display_name: "A".into(),
            description: None,
            config: serde_json::json!({"model": "x", "tools": ["t1"]}),
        };
        let agent = build_tenant_agent("id".into(), "t", &req, 0);
        assert_eq!(agent.config["model"], "x");
        assert_eq!(agent.config["tools"][0], "t1");
    }

    #[test]
    fn resolve_agent_config_uses_updated_at_version_when_missing() {
        let outcome = resolve_agent_config(
            "a",
            "t",
            Some(TenantAgentRowSnapshot {
                enabled: true,
                config: serde_json::json!({"model": "m"}),
                updated_at: 4242,
            }),
        )
        .unwrap();
        if let TenantAgentResolveOutcome::UseTenantDb { config_version, .. } = outcome {
            assert_eq!(config_version, "tenant-db:4242");
        } else {
            panic!("expected tenant db");
        }
    }
}
