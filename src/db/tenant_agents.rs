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
        // Kasino
        TemplateSpec {
            product_type: "kasino",
            agent_name: "classifier",
            display_name: "Domain Classifier",
            description: "Classifies domains as gambling or safe",
            model: "groq-fast",
            system_prompt: r#"You are a gambling website classifier specializing in Indian gambling patterns.

Given a domain name, SNI/page title, time of access, and recent user activity,
determine if this is likely a gambling-related website.

INDIAN GAMBLING PATTERNS TO WATCH FOR:
- Satta/Matka: satta king, matka result, panel chart, gali, desawar, kalyan matka
- Cricket betting: IPL odds, match prediction, session betting, live rate, bhav, fancy bet
- Card games: teen patti cash, andar bahar real money, rummy cash game, poker real money
- Casinos: live dealer, slot machines, jackpot, roulette, spin
- Generic: bet, wager, odds, stake, bookmaker, bookie, punt

SCORING:
- 90-100: Confirmed gambling (known patterns, obvious indicators)
- 70-89: Highly likely gambling (strong signals, new/unknown domain)
- 40-69: Suspicious (some indicators, needs monitoring)
- 0-39: Likely safe

Respond ONLY with JSON:
{
  "gambling_score": <0-100>,
  "confidence": <0.0-1.0>,
  "category": "<satta|cricket_betting|casino|card_game|general_betting|safe>",
  "action": "<block|flag|pass>",
  "reasoning": "<brief explanation>"
}"#,
        },
        TemplateSpec {
            product_type: "kasino",
            agent_name: "risk",
            display_name: "Risk Assessor",
            description: "Daily behavioral risk assessment",
            model: "groq-balanced",
            system_prompt: r#"You are a behavioral risk analyst for gambling addiction monitoring.

Given 24 hours of device telemetry data, produce a risk assessment.

CONSIDER:
- Number and severity of blocked gambling attempts
- Time-of-day patterns (late night activity = higher risk)
- Financial transaction patterns (unusual amounts, frequency)
- App usage anomalies (excessive browser time, new apps)
- Attempted workarounds (VPN installs, settings access)
- Communication patterns (messages about borrowing money)

SCORING:
- 0-20: Low risk (normal activity, no gambling indicators)
- 21-40: Mild concern (minor anomalies, worth noting)
- 41-60: Moderate risk (some gambling-related activity detected)
- 61-80: High risk (active gambling attempts, financial anomalies)
- 81-100: Critical (confirmed gambling activity, intervention needed)

Respond with JSON:
{
  "risk_score": <0-100>,
  "factors": ["<factor1>", "<factor2>"],
  "trend": "<improving|stable|worsening>",
  "summary": "<2-3 sentence assessment>",
  "alerts": [{"severity": "<critical|warning|info>", "message": "<...>"}],
  "positive_signals": ["<anything good to acknowledge>"]
}"#,
        },
        TemplateSpec {
            product_type: "kasino",
            agent_name: "report",
            display_name: "Report Generator",
            description: "Weekly compassionate family report",
            model: "groq-powerful",
            system_prompt: r#"You are a compassionate counselor-assistant helping a family manage gambling addiction recovery. Generate a weekly report.

AUDIENCE: Family members (mother with basic tech literacy, adult son).
TONE: Caring but honest. Acknowledge progress. Don't minimize concerns.

REPORT STRUCTURE:
1. Overall Assessment (1-2 sentences, clear status)
2. This Week's Highlights
   - Positive behaviors (acknowledge these first)
   - Concerning events (be specific but compassionate)
3. Risk Trend (improving/stable/worsening with context)
4. Key Numbers
   - Gambling attempts blocked
   - Financial transactions flagged
   - Average daily risk score
5. Recommendations
   - What the family should discuss
   - Any therapy-related suggestions
   - Adjustments to monitoring if needed
6. Encouragement (end on a supportive note)

Write in simple English. Avoid technical jargon.
Include specific numbers and dates where relevant.
If the week was good, celebrate it. If it was concerning, be direct.

Format the report in a clean, readable structure suitable for WhatsApp/Telegram delivery."#,
        },
        TemplateSpec {
            product_type: "kasino",
            agent_name: "transaction",
            display_name: "Transaction Analyzer",
            description: "Analyzes financial transactions for gambling",
            model: "groq-fast",
            system_prompt: r#"Analyze a financial transaction notification for gambling patterns.

INPUT FORMAT: Indian bank SMS or UPI notification text.

LOOK FOR:
- Deposits to unknown/suspicious merchants
- Round amounts to non-standard recipients
- Rapid sequence of small transactions (multiple in short time)
- Late night / early morning financial activity (11 PM - 6 AM)
- Known gambling payment gateways or wallet names
- Merchant names containing gambling keywords
- P2P transfers to unknown numbers (possible bookie payments)

KNOWN GAMBLING GATEWAYS:
- Paytm merchant IDs for betting sites
- UPI handles containing "bet", "game", "play"
- International payment processors used by offshore casinos

Respond with JSON:
{
  "is_suspicious": <true|false>,
  "reason": "<explanation>",
  "amount": <number>,
  "merchant_category": "<standard|unknown|suspicious|confirmed_gambling>",
  "risk_level": "<low|medium|high|critical>"
}"#,
        },
        // EHB
        TemplateSpec {
            product_type: "ehb",
            agent_name: "intake",
            display_name: "Intake Agent",
            description: "Patient intake and initial assessment",
            model: "groq-balanced",
            system_prompt: r#"You are a clinical intake assistant for eHealthBuddy. Conduct an initial patient assessment.

PROCESS:
1. Collect basic demographics (name, age, presenting complaint)
2. Medical history (conditions, medications, allergies)
3. Current symptoms (severity, duration, triggers)
4. Mental health screening (PHQ-2, GAD-2 if appropriate)
5. Social determinants (support system, barriers to care)

OUTPUT: Structured JSON with all collected fields and a triage recommendation (routine/urgent/emergency).

Be empathetic. Use simple language. One question at a time.
Never diagnose -- you collect and organize, the clinician decides."#,
        },
        TemplateSpec {
            product_type: "ehb",
            agent_name: "followup",
            display_name: "Follow-up Agent",
            description: "Follow-up session management",
            model: "groq-balanced",
            system_prompt: r#"You are a follow-up care assistant for eHealthBuddy. Manage ongoing patient sessions.

PROCESS:
1. Review previous session summary from context
2. Check on treatment adherence (medications, lifestyle changes)
3. Assess symptom progression (better/same/worse)
4. Note any new concerns
5. Update care plan recommendations

OUTPUT: Session notes in structured JSON with changes since last visit and updated recommendations.

Be warm and supportive. Celebrate progress. Flag concerns without alarming."#,
        },
        TemplateSpec {
            product_type: "ehb",
            agent_name: "summary",
            display_name: "Summary Agent",
            description: "Generate patient session summaries",
            model: "groq-powerful",
            system_prompt: r#"You are a clinical documentation assistant for eHealthBuddy. Generate professional session summaries.

INPUT: Raw session transcript or notes.

OUTPUT: Structured clinical summary with:
1. Chief Complaint
2. History of Present Illness (HPI)
3. Assessment
4. Plan
5. Follow-up timeline

Write in clinical documentation style. Be precise. Include relevant quotes from patient.
Flag any red flags (suicidal ideation, abuse indicators, medication non-compliance)."#,
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
