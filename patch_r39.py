#!/usr/bin/env python3
from pathlib import Path

ROOT = Path("/opt/ares/crates/ares-db/src")
SRC = ROOT / "tenant_agents.rs"
HELPERS = ROOT / "tenant_agents_r39_helpers.inc.rs"
TESTS = ROOT / "tenant_agents_r39_tests.inc.rs"

def main() -> None:
    text = SRC.read_text()
    helpers = HELPERS.read_text()
    tests = TESTS.read_text()

    row_old = """fn row_to_tenant_agent(row: &sqlx::postgres::PgRow) -> TenantAgent {
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
}"""

    row_new = helpers.rstrip() + "\n"
    if row_old not in text:
        if "pub struct TenantAgentConfig" in text:
            print("helpers already applied")
        else:
            raise SystemExit("row_to_tenant_agent block not found")
    else:
        text = text.replace(row_old, row_new)
        text = text.replace("row_to_tenant_agent", "agent_from_row")

    create_old = """pub async fn create_tenant_agent(
    pool: &PgPool,
    tenant_id: &str,
    req: CreateTenantAgentRequest,
) -> Result<TenantAgent> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ts();

    sqlx::query(INSERT_TENANT_AGENT_SQL)"""

    create_new = create_old.replace(
        "    let id = uuid::Uuid::new_v4().to_string();",
        "    prepare_create_tenant_agent(&req)?;\n    let id = uuid::Uuid::new_v4().to_string();",
    )
    if create_old in text:
        text = text.replace(create_old, create_new)

    update_old = """    let display_name = req.display_name.unwrap_or(current.display_name);
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
    .map_err(|e| AppError::Database(e.to_string()))?;"""

    update_new = """    let merged = merge_tenant_agent_update(&current, &req, now);
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
    .map_err(|e| AppError::Database(e.to_string()))?;"""

    if update_old in text:
        text = text.replace(update_old, update_new)

    if "fn tenant_agent_config_serde_roundtrip" not in text:
        marker = "\n}\n\n// =============================================================================\n// Template operations"
        if marker not in text:
            raise SystemExit("tests insertion marker missing")
        text = text.replace(marker, "\n" + tests.rstrip() + marker)

    SRC.write_text(text)
    count = text.count("#[test]") + text.count("#[tokio::test]")
    print(f"wrote {SRC} ({len(text.splitlines())} lines, {count} tests)")
    if count < 64:
        raise SystemExit(f"need >=64 tests, got {count}")

if __name__ == "__main__":
    main()
