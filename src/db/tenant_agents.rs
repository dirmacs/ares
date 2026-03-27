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

    rows.iter()
        .map(|row| {
            Ok(TenantAgent {
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
        })
        .collect()
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

    Ok(TenantAgent {
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

pub async fn create_tenant_agent(
    pool: &PgPool,
    tenant_id: &str,
    req: CreateTenantAgentRequest,
) -> Result<TenantAgent> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ts();

    sqlx::query(
        "INSERT INTO tenant_agents (id, tenant_id, agent_name, display_name, description, config, enabled, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, true, $7, $7)"
    )
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

    get_tenant_agent(pool, tenant_id, &req.agent_name).await
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

    get_tenant_agent(pool, tenant_id, agent_name).await
}

pub async fn delete_tenant_agent(pool: &PgPool, tenant_id: &str, agent_name: &str) -> Result<()> {
    let result = sqlx::query("DELETE FROM tenant_agents WHERE tenant_id = $1 AND agent_name = $2")
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
