//! Custom skills and connector configurations.
//!
//! Provides CRUD for `skills` and `connectors` tables (migration 019).

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skill {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub skill_type: String,
    pub steps: serde_json::Value,
    pub input_schema: Option<serde_json::Value>,
    pub output_schema: Option<serde_json::Value>,
    pub tools: Option<Vec<String>>,
    pub is_public: bool,
    pub created_by: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Connector {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub service_type: String,
    pub auth_config: serde_json::Value,
    pub endpoints: serde_json::Value,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateSkillRequest {
    #[serde(default = "default_tenant_id")]
    pub tenant_id: String,
    pub name: String,
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default = "default_skill_type")]
    pub skill_type: String,
    pub steps: serde_json::Value,
    pub input_schema: Option<serde_json::Value>,
    pub output_schema: Option<serde_json::Value>,
    pub tools: Option<Vec<String>>,
    #[serde(default = "default_false")]
    pub is_public: bool,
    pub created_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateConnectorRequest {
    pub tenant_id: String,
    pub name: String,
    pub service_type: String,
    pub auth_config: serde_json::Value,
    pub endpoints: serde_json::Value,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_tenant_id() -> String {
    "default".to_string()
}

fn default_skill_type() -> String {
    "workflow".to_string()
}
fn default_true() -> bool {
    true
}
fn default_false() -> bool {
    false
}

// =============================================================================
// Skill Store
// =============================================================================

pub struct SkillStore<'a> {
    pool: &'a PgPool,
}

impl<'a> SkillStore<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn list_skills(&self, tenant_id: Option<&str>) -> Result<Vec<Skill>> {
        let rows = if let Some(tid) = tenant_id {
            sqlx::query(
                "SELECT id, tenant_id, name, display_name, description, skill_type, steps, \
                        input_schema, output_schema, tools, is_public, created_by, created_at, updated_at \
                 FROM skills WHERE tenant_id = $1 ORDER BY name",
            )
            .bind(tid)
            .fetch_all(self.pool)
            .await
            .map_err(sqlx_err)?
        } else {
            sqlx::query(
                "SELECT id, tenant_id, name, display_name, description, skill_type, steps, \
                        input_schema, output_schema, tools, is_public, created_by, created_at, updated_at \
                 FROM skills ORDER BY name",
            )
            .fetch_all(self.pool)
            .await
            .map_err(sqlx_err)?
        };
        rows.iter().map(row_to_skill).collect()
    }

    pub async fn get_skill(&self, id: &str) -> Result<Option<Skill>> {
        let row = sqlx::query(
            "SELECT id, tenant_id, name, display_name, description, skill_type, steps, \
                    input_schema, output_schema, tools, is_public, created_by, created_at, updated_at \
             FROM skills WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;
        row.map(|r| row_to_skill(&r)).transpose()
    }

    pub async fn create_skill(&self, req: &CreateSkillRequest) -> Result<Skill> {
        validate_skill_request(req)?;
        let now = now_ts();
        let id = uuid::Uuid::new_v4().to_string();

        let row = sqlx::query(
            "INSERT INTO skills \
                (id, tenant_id, name, display_name, description, skill_type, steps, \
                 input_schema, output_schema, tools, is_public, created_by, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13) \
             RETURNING id, tenant_id, name, display_name, description, skill_type, steps, \
                       input_schema, output_schema, tools, is_public, created_by, created_at, updated_at",
        )
        .bind(&id)
        .bind(&req.tenant_id)
        .bind(&req.name)
        .bind(&req.display_name)
        .bind(&req.description)
        .bind(&req.skill_type)
        .bind(&req.steps)
        .bind(&req.input_schema)
        .bind(&req.output_schema)
        .bind(&req.tools.as_ref().map(|v| v.as_slice()))
        .bind(req.is_public)
        .bind(&req.created_by)
        .bind(now)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_skill(&row)
    }

    pub async fn update_skill(&self, id: &str, req: &CreateSkillRequest) -> Result<Option<Skill>> {
        validate_skill_request(req)?;
        let now = now_ts();

        let row = sqlx::query(
            "UPDATE skills SET \
                tenant_id = $2, name = $3, display_name = $4, description = $5, \
                skill_type = $6, steps = $7, input_schema = $8, output_schema = $9, \
                tools = $10, is_public = $11, created_by = COALESCE($12, created_by), updated_at = $13 \
             WHERE id = $1 \
             RETURNING id, tenant_id, name, display_name, description, skill_type, steps, \
                       input_schema, output_schema, tools, is_public, created_by, created_at, updated_at",
        )
        .bind(id)
        .bind(&req.tenant_id)
        .bind(&req.name)
        .bind(&req.display_name)
        .bind(&req.description)
        .bind(&req.skill_type)
        .bind(&req.steps)
        .bind(&req.input_schema)
        .bind(&req.output_schema)
        .bind(&req.tools.as_ref().map(|v| v.as_slice()))
        .bind(req.is_public)
        .bind(&req.created_by)
        .bind(now)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        row.map(|r| row_to_skill(&r)).transpose()
    }

    pub async fn delete_skill(&self, id: &str) -> Result<u64> {
        let res = sqlx::query("DELETE FROM skills WHERE id = $1")
            .bind(id)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;
        Ok(res.rows_affected())
    }
}

// =============================================================================
// Connector Store
// =============================================================================

pub struct ConnectorStore<'a> {
    pool: &'a PgPool,
}

impl<'a> ConnectorStore<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn list_connectors(&self, tenant_id: &str) -> Result<Vec<Connector>> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, name, service_type, auth_config, endpoints, enabled, \
                    created_at, updated_at \
             FROM connectors WHERE tenant_id = $1 ORDER BY name",
        )
        .bind(tenant_id)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;
        rows.iter().map(row_to_connector).collect()
    }

    pub async fn create_connector(&self, req: &CreateConnectorRequest) -> Result<Connector> {
        if req.name.is_empty() {
            return Err(AppError::InvalidInput(
                "connector name must not be empty".into(),
            ));
        }
        if req.service_type.is_empty() {
            return Err(AppError::InvalidInput(
                "connector service_type must not be empty".into(),
            ));
        }
        validate_service_type(&req.service_type)?;
        let now = now_ts();
        let id = uuid::Uuid::new_v4().to_string();

        let row = sqlx::query(
            "INSERT INTO connectors \
                (id, tenant_id, name, service_type, auth_config, endpoints, enabled, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8) \
             RETURNING id, tenant_id, name, service_type, auth_config, endpoints, enabled, created_at, updated_at",
        )
        .bind(&id)
        .bind(&req.tenant_id)
        .bind(&req.name)
        .bind(&req.service_type)
        .bind(&req.auth_config)
        .bind(&req.endpoints)
        .bind(req.enabled)
        .bind(now)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_connector(&row)
    }

    pub async fn delete_connector(&self, id: &str) -> Result<u64> {
        let res = sqlx::query("DELETE FROM connectors WHERE id = $1")
            .bind(id)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;
        Ok(res.rows_affected())
    }
}

// =============================================================================
// Row mappers
// =============================================================================

fn row_to_skill(row: &sqlx::postgres::PgRow) -> Result<Skill> {
    Ok(Skill {
        id: row.try_get("id").map_err(sqlx_err)?,
        tenant_id: row.try_get("tenant_id").map_err(sqlx_err)?,
        name: row.try_get("name").map_err(sqlx_err)?,
        display_name: row.try_get("display_name").map_err(sqlx_err)?,
        description: row.try_get("description").map_err(sqlx_err)?,
        skill_type: row.try_get("skill_type").map_err(sqlx_err)?,
        steps: row.try_get("steps").map_err(sqlx_err)?,
        input_schema: row.try_get("input_schema").map_err(sqlx_err)?,
        output_schema: row.try_get("output_schema").map_err(sqlx_err)?,
        tools: row.try_get("tools").map_err(sqlx_err)?,
        is_public: row.try_get("is_public").map_err(sqlx_err)?,
        created_by: row.try_get("created_by").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
        updated_at: row.try_get("updated_at").map_err(sqlx_err)?,
    })
}

fn row_to_connector(row: &sqlx::postgres::PgRow) -> Result<Connector> {
    Ok(Connector {
        id: row.try_get("id").map_err(sqlx_err)?,
        tenant_id: row.try_get("tenant_id").map_err(sqlx_err)?,
        name: row.try_get("name").map_err(sqlx_err)?,
        service_type: row.try_get("service_type").map_err(sqlx_err)?,
        auth_config: row.try_get("auth_config").map_err(sqlx_err)?,
        endpoints: row.try_get("endpoints").map_err(sqlx_err)?,
        enabled: row.try_get("enabled").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
        updated_at: row.try_get("updated_at").map_err(sqlx_err)?,
    })
}

fn sqlx_err(e: sqlx::Error) -> AppError {
    AppError::Database(e.to_string())
}

fn validate_skill_request(req: &CreateSkillRequest) -> Result<()> {
    if req.name.is_empty() {
        return Err(AppError::InvalidInput(
            "skill name must not be empty".into(),
        ));
    }
    if req.display_name.is_empty() {
        return Err(AppError::InvalidInput(
            "skill display_name must not be empty".into(),
        ));
    }
    validate_skill_type(&req.skill_type)
}

fn validate_skill_type(t: &str) -> Result<()> {
    match t {
        "workflow" | "connector" | "composite" => Ok(()),
        _ => Err(AppError::InvalidInput(format!(
            "invalid skill_type: {t}. Must be one of: workflow, connector, composite"
        ))),
    }
}

fn validate_service_type(t: &str) -> Result<()> {
    const VALID: &[&str] = &[
        "google_drive",
        "slack",
        "linkedin",
        "hubspot",
        "salesforce",
        "email",
        "custom",
    ];
    if VALID.contains(&t) {
        Ok(())
    } else {
        Err(AppError::InvalidInput(format!(
            "invalid service_type: {t}. Must be one of: {}",
            VALID.join(", ")
        )))
    }
}

// =============================================================================
// Seed default skills
// =============================================================================

/// Seeds four pre-built skills. Idempotent — uses ON CONFLICT DO NOTHING.
pub async fn seed_default_skills(pool: &PgPool) -> Result<()> {
    let now = now_ts();
    let skills: Vec<(&str, &str, &str, &str, serde_json::Value, Option<Vec<&str>>)> = vec![
        (
            "web_research",
            "Web Research",
            "Search the web, scrape pages, and summarize findings",
            "workflow",
            serde_json::json!([
                { "type": "tool_call", "tool": "web_search", "input": "{{query}}" },
                { "type": "tool_call", "tool": "web_scrape", "input": "{{urls}}" },
                { "type": "llm_call", "prompt": "Summarize the following content: {{content}}" }
            ]),
            Some(vec!["web_search", "web_scrape"]),
        ),
        (
            "document_extraction",
            "Document Extraction",
            "Read documents, extract structured data, and write output",
            "workflow",
            serde_json::json!([
                { "type": "tool_call", "tool": "chunk_reader", "input": "{{document}}" },
                { "type": "llm_call", "prompt": "Extract structured data: {{chunks}}" },
                { "type": "tool_call", "tool": "write", "input": "{{extracted}}" }
            ]),
            Some(vec!["chunk_reader", "write"]),
        ),
        (
            "linkedin_enrichment",
            "LinkedIn Enrichment",
            "Lookup LinkedIn profiles and update entity records",
            "workflow",
            serde_json::json!([
                { "type": "tool_call", "tool": "linkedin_lookup", "input": "{{profile_url}}" },
                { "type": "tool_call", "tool": "entity_update", "input": "{{profile_data}}" }
            ]),
            Some(vec!["linkedin_lookup", "entity_update"]),
        ),
        (
            "customer_onboarding",
            "Customer Onboarding",
            "Read form data, send welcome email, and create CRM record",
            "workflow",
            serde_json::json!([
                { "type": "tool_call", "tool": "form_read", "input": "{{form_id}}" },
                { "type": "tool_call", "tool": "email_send", "input": "{{email_payload}}" },
                { "type": "tool_call", "tool": "crm_create", "input": "{{customer_data}}" }
            ]),
            Some(vec!["form_read", "email_send", "crm_create"]),
        ),
    ];

    for (name, display_name, description, skill_type, steps, tools) in skills {
        sqlx::query(
            "INSERT INTO skills \
                (id, tenant_id, name, display_name, description, skill_type, steps, \
                 input_schema, output_schema, tools, is_public, created_at, updated_at) \
             VALUES (gen_random_uuid()::text, 'system', $1, $2, $3, $4, $5, NULL, NULL, $6, true, $7, $7) \
             ON CONFLICT (tenant_id, name) DO NOTHING",
        )
        .bind(name)
        .bind(display_name)
        .bind(description)
        .bind(skill_type)
        .bind(&steps)
        .bind(tools.as_ref().map(|v| v.as_slice()))
        .bind(now)
        .execute(pool)
        .await
        .map_err(sqlx_err)?;
    }
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_ts_returns_positive_value() {
        assert!(now_ts() > 0);
    }

    #[test]
    fn create_skill_request_defaults_missing_tenant() {
        let req: CreateSkillRequest = serde_json::from_value(serde_json::json!({
            "name": "demo",
            "display_name": "Demo",
            "skill_type": "workflow",
            "steps": []
        }))
        .expect("request should deserialize without tenant_id");
        assert_eq!(req.tenant_id, "default");
    }

    #[test]
    fn validate_skill_type_accepts_known() {
        assert!(validate_skill_type("workflow").is_ok());
        assert!(validate_skill_type("connector").is_ok());
        assert!(validate_skill_type("composite").is_ok());
        assert!(validate_skill_type("unknown").is_err());
    }

    #[test]
    fn validate_service_type_accepts_known() {
        assert!(validate_service_type("slack").is_ok());
        assert!(validate_service_type("custom").is_ok());
        assert!(validate_service_type("unknown").is_err());
    }
}
