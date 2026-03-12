//! Kasino (Guardian) API Handlers
//! Device monitoring, classification, and family dashboard endpoints

use crate::agents::Agent;
use crate::auth::middleware::AuthUser;
use crate::models::tenant::TenantContext;
use crate::types::{AppError, Result, AgentContext};
use crate::AppState;
use axum::{
    extract::FromRequestParts,
    extract::{Json, Path, Query, State},
    http::StatusCode,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

// =============================================================================
// Custom Extractor for Tenant ID
// =============================================================================

#[derive(Clone)]
pub struct TenantId(pub String);

impl<S> FromRequestParts<S> for TenantId
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, axum::Json<serde_json::Value>);

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        // First try TenantContext (API key path)
        if let Some(ctx) = parts.extensions.get::<TenantContext>() {
            return Ok(TenantId(ctx.tenant_id.clone()));
        }

        // Then try AuthUser (JWT path)
        if let Some(auth) = parts.extensions.get::<AuthUser>() {
            return Ok(TenantId(auth.0.sub.clone()));
        }

        Err((
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error": "No tenant context found"})),
        ))
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

fn default_agent_context() -> AgentContext {
    AgentContext {
        user_id: String::new(),
        session_id: String::new(),
        conversation_history: vec![],
        user_memory: None,
    }
}

// =============================================================================
// Request/Response Types
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct ClassifyRequest {
    pub domain: String,
    pub sni: Option<String>,
    pub time: String,
    pub recent_activity: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct ClassifyResponse {
    pub gambling_score: f32,
    pub confidence: f32,
    pub category: String,
    pub action: String,
    pub reasoning: String,
}

#[derive(Debug, Deserialize)]
pub struct TransactionRequest {
    pub device_id: String,
    pub amount: f64,
    pub merchant: String,
    pub method: String,
    pub raw_text: String,
    pub time: String,
}

#[derive(Debug, Serialize)]
pub struct TransactionResponse {
    pub is_suspicious: bool,
    pub reason: String,
    pub amount: f64,
    pub merchant_category: String,
    pub risk_level: String,
}

#[derive(Debug, Deserialize)]
pub struct EventRequest {
    pub device_id: String,
    pub event_type: String,
    pub severity: String,
    pub source: String,
    pub domain: Option<String>,
    pub app_package: Option<String>,
    pub content: Option<String>,
    pub gambling_score: Option<f32>,
    pub action_taken: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct EventResponse {
    pub event_id: String,
    pub action_taken: String,
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    pub device_id: Option<String>,
    pub severity: Option<String>,
    pub event_type: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct RiskScoreResponse {
    pub device_id: String,
    pub date: String,
    pub score: f32,
    pub factors: Vec<String>,
    pub trend: String,
    pub event_count: i64,
    pub blocked_count: i64,
}

#[derive(Debug, Serialize)]
pub struct DashboardResponse {
    pub devices: Vec<DeviceStatus>,
    pub recent_events: Vec<EventSummary>,
    pub risk_summary: RiskSummary,
}

#[derive(Debug, Serialize)]
pub struct DeviceStatus {
    pub id: String,
    pub name: String,
    pub status: String,
    pub block_mode: String,
    pub last_seen: String,
    pub vpn_active: bool,
    pub current_risk_score: f32,
}

#[derive(Debug, Serialize)]
pub struct EventSummary {
    pub id: String,
    pub timestamp: String,
    pub severity: String,
    pub event_type: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct RiskSummary {
    pub average_score: f32,
    pub critical_count: i64,
    pub warning_count: i64,
    pub info_count: i64,
    pub trend: String,
}

#[derive(Debug, Deserialize)]
pub struct DeviceCommandRequest {
    pub device_id: String,
    pub command: String,
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct RegisterDeviceRequest {
    pub name: String,
    pub device_token: Option<String>,
    pub block_mode: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterDeviceResponse {
    pub id: String,
    pub name: String,
    pub status: String,
    pub block_mode: String,
}

#[derive(Debug, Serialize)]
pub struct Rule {
    pub id: String,
    pub rule_type: String,
    pub pattern: String,
    pub action: String,
    pub source: String,
    pub enabled: bool,
    pub hits: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateRuleRequest {
    pub rule_type: String,
    pub pattern: String,
    pub action: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRuleRequest {
    pub rule_type: Option<String>,
    pub pattern: Option<String>,
    pub action: Option<String>,
    pub enabled: Option<bool>,
}

// =============================================================================
// Handler Functions
// =============================================================================

pub async fn classify_domain(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
    Json(payload): Json<ClassifyRequest>,
) -> Result<Json<ClassifyResponse>> {
    
    let domain_lower = payload.domain.to_lowercase();
    
    let (score, category, action) = if domain_lower.contains("satta") || domain_lower.contains("matka") {
        (95.0, "satta", "block")
    } else if domain_lower.contains("bet") || domain_lower.contains("casino") {
        (85.0, "general_betting", "block")
    } else if domain_lower.contains("cricket") && domain_lower.contains("odds") {
        (80.0, "cricket_betting", "block")
    } else {
        (15.0, "safe", "pass")
    };
    
    let gambling_score = score;
    
    if gambling_score > 40.0 {
        let agent_registry = state.agent_registry.clone();
        let domain = payload.domain.clone();
        let tenant_id_clone = tenant_id.clone();
        
        tokio::spawn(async move {
            if let Ok(agent) = agent_registry.create_agent("kasino-classifier").await {
                let input = format!("Classify domain: {}", domain);
                let context = default_agent_context();
                if let Ok(result) = agent.execute(&input, &context).await {
                    tracing::info!("Kasino classifier result for {}: {}", domain, result);
                }
            }
        });
    }
    
    Ok(Json(ClassifyResponse {
        gambling_score: score,
        confidence: 0.85,
        category: category.to_string(),
        action: action.to_string(),
        reasoning: format!("Domain '{}' matched pattern analysis", payload.domain),
    }))
}

pub async fn analyze_transaction(
    State(state): State<AppState>,
    TenantId(_tenant_id): TenantId,
    Json(payload): Json<TransactionRequest>,
) -> Result<Json<TransactionResponse>> {
    
    let is_suspicious = payload.amount >= 1000.0 && 
        (payload.time.contains("23:") || payload.time.contains("00:") || payload.time.contains("01:") || payload.time.contains("02:"));
    
    let risk_level = if is_suspicious { "high" } else { "low" };
    
    let agent_registry = state.agent_registry.clone();
    let device_id = payload.device_id.clone();
    let amount = payload.amount;
    let merchant = payload.merchant.clone();
    let method = payload.method.clone();
    
    tokio::spawn(async move {
        if let Ok(agent) = agent_registry.create_agent("kasino-transaction").await {
            let input = format!("Analyze transaction: {} {} from {}", amount, method, merchant);
            let context = default_agent_context();
            if let Ok(result) = agent.execute(&input, &context).await {
                tracing::info!("Kasino transaction analysis for device {}: {}", device_id, result);
            }
        }
    });
    
    Ok(Json(TransactionResponse {
        is_suspicious,
        reason: if is_suspicious { 
            "Round amount at suspicious hour".to_string() 
        } else { 
            "Normal transaction".to_string() 
        },
        amount: payload.amount,
        merchant_category: "unknown".to_string(),
        risk_level: risk_level.to_string(),
    }))
}

pub async fn log_event(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
    Json(payload): Json<EventRequest>,
) -> Result<Json<EventResponse>> {
    let event_id = Uuid::new_v4().to_string();
    let event_id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    
    sqlx::query(
        "INSERT INTO kasno_events (id, tenant_id, device_id, event_type, severity, source, domain, app_package, content, gambling_score, action_taken, metadata, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"
    )
    .bind(&event_id)
    .bind(&tenant_id)
    .bind(&payload.device_id)
    .bind(&payload.event_type)
    .bind(&payload.severity)
    .bind(&payload.source)
    .bind(&payload.domain)
    .bind(&payload.app_package)
    .bind(&payload.content)
    .bind(payload.gambling_score)
    .bind(&payload.action_taken)
    .bind(payload.metadata.map(|v| v.to_string()))
    .bind(now)
    .execute(state.tenant_db.pool())
    .await
    .map_err(|e| AppError::Database(format!("Failed to log event: {}", e)))?;
    
    if payload.severity == "critical" {
        tracing::warn!("Critical kasno event logged for device {}: {:?}", payload.device_id, payload.event_type);
    }
    
    Ok(Json(EventResponse {
        event_id,
        action_taken: "stored".to_string(),
    }))
}

pub async fn log_events_bulk(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
    Json(payload): Json<Vec<EventRequest>>,
) -> Result<Json<Vec<EventResponse>>> {
    let mut responses: Vec<EventResponse> = Vec::new();
    let now = Utc::now().timestamp();
    
    for event in payload {
        let event_id = Uuid::new_v4().to_string();
        
        sqlx::query(
            "INSERT INTO kasno_events (id, tenant_id, device_id, event_type, severity, source, domain, app_package, content, gambling_score, action_taken, metadata, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"
        )
        .bind(&event_id)
        .bind(&tenant_id)
        .bind(&event.device_id)
        .bind(&event.event_type)
        .bind(&event.severity)
        .bind(&event.source)
        .bind(&event.domain)
        .bind(&event.app_package)
        .bind(&event.content)
        .bind(event.gambling_score)
        .bind(&event.action_taken)
        .bind(event.metadata.map(|v| v.to_string()))
        .bind(now)
        .execute(state.tenant_db.pool())
        .await
        .map_err(|e| AppError::Database(format!("Failed to log event: {}", e)))?;
        
        responses.push(EventResponse {
            event_id,
            action_taken: "stored".to_string(),
        });
    }
    
    Ok(Json(responses))
}

pub async fn query_events(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
    Query(query_params): Query<EventsQuery>,
) -> Result<Json<Vec<EventSummary>>> {
    
    let mut query_str = "SELECT id, device_id, event_type, severity, content, created_at FROM kasno_events WHERE tenant_id = $1".to_string();
    let mut param_idx: i32 = 1;
    
    if query_params.device_id.is_some() {
        param_idx += 1;
        query_str.push_str(&format!(" AND device_id = ${}", param_idx));
    }
    if query_params.severity.is_some() {
        param_idx += 1;
        query_str.push_str(&format!(" AND severity = ${}", param_idx));
    }
    if query_params.event_type.is_some() {
        param_idx += 1;
        query_str.push_str(&format!(" AND event_type = ${}", param_idx));
    }
    if query_params.from.is_some() {
        param_idx += 1;
        query_str.push_str(&format!(" AND created_at >= ${}", param_idx));
    }
    if query_params.to.is_some() {
        param_idx += 1;
        query_str.push_str(&format!(" AND created_at <= ${}", param_idx));
    }
    
    query_str.push_str(" ORDER BY created_at DESC");
    
    let limit = query_params.limit.unwrap_or(100);
    query_str.push_str(&format!(" LIMIT {}", limit));
    
    if let Some(offset) = query_params.offset {
        query_str.push_str(&format!(" OFFSET {}", offset));
    }
    
    let mut q = sqlx::query(&query_str);
    q = q.bind(&tenant_id);
    if let Some(ref device_id) = query_params.device_id {
        q = q.bind(device_id.clone());
    }
    if let Some(ref severity) = query_params.severity {
        q = q.bind(severity.clone());
    }
    if let Some(ref event_type) = query_params.event_type {
        q = q.bind(event_type.clone());
    }
    if let Some(ref from) = query_params.from {
        q = q.bind(from.parse::<i64>().unwrap_or(0));
    }
    if let Some(ref to) = query_params.to {
        q = q.bind(to.parse::<i64>().unwrap_or(i64::MAX));
    }
    
    let rows = q.fetch_all(state.tenant_db.pool()).await
        .map_err(|e| AppError::Database(format!("Failed to query events: {}", e)))?;
    
    let events: Vec<EventSummary> = rows.iter().map(|row| {
        let id: String = row.get(0);
        let _device_id: String = row.get(1);
        let event_type: String = row.get(2);
        let severity: String = row.get(3);
        let content: Option<String> = row.get(4);
        let created_at: i64 = row.get(5);
        
        EventSummary {
            id,
            timestamp: chrono::DateTime::from_timestamp(created_at, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
            severity,
            event_type,
            description: content.unwrap_or_default(),
        }
    }).collect();
    
    Ok(Json(events))
}

pub async fn get_risk_score(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
    Path(device_id): Path<String>,
) -> Result<Json<RiskScoreResponse>> {
    
    let row = sqlx::query(
        "SELECT id, score_date, risk_score, factors, trend FROM kasno_risk_scores WHERE device_id = $1 AND tenant_id = $2 ORDER BY score_date DESC LIMIT 1"
    )
    .bind(&device_id)
    .bind(&tenant_id)
    .fetch_optional(state.tenant_db.pool())
    .await
    .map_err(|e| AppError::Database(format!("Failed to get risk score: {}", e)))?;
    
    if let Some(row) = row {
        let factors: Option<String> = row.get(3);
        let factors_vec: Vec<String> = factors
            .and_then(|f| serde_json::from_str(&f).ok())
            .unwrap_or_default();
        
        let event_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM kasno_events WHERE device_id = $1 AND created_at > $2"
        )
        .bind(&device_id)
        .bind(Utc::now().timestamp() - 86400)
        .fetch_one(state.tenant_db.pool())
        .await
        .unwrap_or(0);
        
        let blocked_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM kasno_events WHERE device_id = $1 AND event_type = 'domain_blocked' AND created_at > $2"
        )
        .bind(&device_id)
        .bind(Utc::now().timestamp() - 86400)
        .fetch_one(state.tenant_db.pool())
        .await
        .unwrap_or(0);
        
        return Ok(Json(RiskScoreResponse {
            device_id,
            date: row.get(1),
            score: row.get(2),
            factors: factors_vec,
            trend: row.get::<String, _>(4),
            event_count,
            blocked_count,
        }));
    }
    
    let critical_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM kasno_events WHERE device_id = $1 AND severity = 'critical' AND created_at > $2"
    )
    .bind(&device_id)
    .bind(Utc::now().timestamp() - 86400)
    .fetch_one(state.tenant_db.pool())
    .await
    .unwrap_or(0);
    
    let warning_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM kasno_events WHERE device_id = $1 AND severity = 'warning' AND created_at > $2"
    )
    .bind(&device_id)
    .bind(Utc::now().timestamp() - 86400)
    .fetch_one(state.tenant_db.pool())
    .await
    .unwrap_or(0);
    
    let score = ((critical_count as f32) * 30.0 + (warning_count as f32) * 10.0).min(100.0);
    
    Ok(Json(RiskScoreResponse {
        device_id,
        date: Utc::now().format("%Y-%m-%d").to_string(),
        score,
        factors: vec!["No stored risk score - computed from recent events".to_string()],
        trend: "unknown".to_string(),
        event_count: critical_count + warning_count,
        blocked_count: 0,
    }))
}

pub async fn get_dashboard(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
) -> Result<Json<DashboardResponse>> {
    
    let device_rows = sqlx::query(
        "SELECT id, name, status, block_mode, last_seen FROM kasno_devices WHERE tenant_id = $1"
    )
    .bind(&tenant_id)
    .fetch_all(state.tenant_db.pool())
    .await
    .map_err(|e| AppError::Database(format!("Failed to list devices: {}", e)))?;
    
    let mut devices = Vec::new();
    for row in device_rows {
        let device_id: String = row.get(0);
        
        let risk_score: f32 = sqlx::query_scalar(
            "SELECT risk_score FROM kasno_risk_scores WHERE device_id = $1 ORDER BY score_date DESC LIMIT 1"
        )
        .bind(&device_id)
        .fetch_optional(state.tenant_db.pool())
        .await
        .unwrap_or(Some(0.0))
        .unwrap_or(0.0);
        
        devices.push(DeviceStatus {
            id: row.get(0),
            name: row.get(1),
            status: row.get(2),
            block_mode: row.get(3),
            last_seen: row.get::<Option<i64>, _>(4)
                .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
            vpn_active: false,
            current_risk_score: risk_score,
        });
    }
    
    let event_rows = sqlx::query(
        "SELECT id, event_type, severity, content, created_at FROM kasno_events WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT 20"
    )
    .bind(&tenant_id)
    .fetch_all(state.tenant_db.pool())
    .await
    .map_err(|e| AppError::Database(format!("Failed to query events: {}", e)))?;
    
    let recent_events: Vec<EventSummary> = event_rows.iter().map(|row| {
        let id: String = row.get(0);
        let event_type: String = row.get(1);
        let severity: String = row.get(2);
        let content: Option<String> = row.get(3);
        let created_at: i64 = row.get(4);
        
        EventSummary {
            id,
            timestamp: chrono::DateTime::from_timestamp(created_at, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
            severity,
            event_type,
            description: content.unwrap_or_default(),
        }
    }).collect();
    
    let yesterday = Utc::now().timestamp() - 86400;
    
    let critical_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM kasno_events WHERE tenant_id = $1 AND severity = 'critical' AND created_at > $2"
    )
    .bind(&tenant_id)
    .bind(yesterday)
    .fetch_one(state.tenant_db.pool())
    .await
    .unwrap_or(0);
    
    let warning_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM kasno_events WHERE tenant_id = $1 AND severity = 'warning' AND created_at > $2"
    )
    .bind(&tenant_id)
    .bind(yesterday)
    .fetch_one(state.tenant_db.pool())
    .await
    .unwrap_or(0);
    
    let info_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM kasno_events WHERE tenant_id = $1 AND severity = 'info' AND created_at > $2"
    )
    .bind(&tenant_id)
    .bind(yesterday)
    .fetch_one(state.tenant_db.pool())
    .await
    .unwrap_or(0);
    
    let avg_score: f32 = sqlx::query_scalar(
        "SELECT AVG(risk_score) FROM kasno_risk_scores WHERE tenant_id = $1 AND computed_at > $2"
    )
    .bind(&tenant_id)
    .bind(Utc::now().timestamp() - 604800)
    .fetch_one(state.tenant_db.pool())
    .await
    .unwrap_or(Some(0.0))
    .unwrap_or(0.0);
    
    Ok(Json(DashboardResponse {
        devices,
        recent_events,
        risk_summary: RiskSummary {
            average_score: avg_score,
            critical_count,
            warning_count,
            info_count,
            trend: if avg_score > 50.0 { "increasing".to_string() } else { "stable".to_string() },
        },
    }))
}

pub async fn send_device_command(
    State(_state): State<AppState>,
    TenantId(_tenant_id): TenantId,
    Json(payload): Json<DeviceCommandRequest>,
) -> Result<StatusCode> {
    tracing::info!("Device command '{}' sent to device {}", payload.command, payload.device_id);
    
    Ok(StatusCode::ACCEPTED)
}

pub async fn get_weekly_report(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>> {
    let device_id = params.get("device_id").cloned().unwrap_or_default();
    
    let week_ago = Utc::now().timestamp() - 604800;
    
    let risk_rows = if !device_id.is_empty() {
        sqlx::query(
            "SELECT AVG(risk_score), COUNT(*) FROM kasno_risk_scores WHERE device_id = $1 AND tenant_id = $2 AND computed_at > $3"
        )
        .bind(&device_id)
        .bind(&tenant_id)
        .bind(week_ago)
        .fetch_optional(state.tenant_db.pool())
        .await
        .ok()
    } else {
        None
    };
    
    let avg_score: f32 = if let Some(Some(row)) = risk_rows {
        row.get::<Option<f32>, _>(0).unwrap_or(0.0)
    } else {
        0.0
    };
    
    let critical_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM kasno_events WHERE tenant_id = $1 AND severity = 'critical' AND created_at > $2"
    )
    .bind(&tenant_id)
    .bind(week_ago)
    .fetch_one(state.tenant_db.pool())
    .await
    .unwrap_or(0);
    
    let warning_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM kasno_events WHERE tenant_id = $1 AND severity = 'warning' AND created_at > $2"
    )
    .bind(&tenant_id)
    .bind(week_ago)
    .fetch_one(state.tenant_db.pool())
    .await
    .unwrap_or(0);
    
    let info_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM kasno_events WHERE tenant_id = $1 AND severity = 'info' AND created_at > $2"
    )
    .bind(&tenant_id)
    .bind(week_ago)
    .fetch_one(state.tenant_db.pool())
    .await
    .unwrap_or(0);
    
    let blocked_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM kasno_events WHERE tenant_id = $1 AND event_type = 'domain_blocked' AND created_at > $2"
    )
    .bind(&tenant_id)
    .bind(week_ago)
    .fetch_one(state.tenant_db.pool())
    .await
    .unwrap_or(0);
    
    let total_events = critical_count + warning_count + info_count;
    
    let week_start = (Utc::now() - chrono::Duration::days(7)).format("%Y-%m-%d").to_string();
    let week_end = Utc::now().format("%Y-%m-%d").to_string();
    
    Ok(Json(json!({
        "device_id": device_id,
        "week_start": week_start,
        "week_end": week_end,
        "avg_score": avg_score,
        "event_counts": {
            "critical": critical_count,
            "warning": warning_count,
            "info": info_count,
            "total": total_events
        },
        "blocked_count": blocked_count,
        "total_events": total_events,
    })))
}

pub async fn list_rules(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
) -> Result<Json<Vec<Rule>>> {
    
    let rows = sqlx::query(
        "SELECT id, rule_type, pattern, action, source, enabled, hits FROM kasno_rules WHERE tenant_id = $1 ORDER BY created_at DESC"
    )
    .bind(&tenant_id)
    .fetch_all(state.tenant_db.pool())
    .await
    .map_err(|e| AppError::Database(format!("Failed to list rules: {}", e)))?;
    
    let rules: Vec<Rule> = rows.iter().map(|row| {
        Rule {
            id: row.get(0),
            rule_type: row.get(1),
            pattern: row.get(2),
            action: row.get(3),
            source: row.get(4),
            enabled: row.get::<i32, _>(5) != 0,
            hits: row.get(6),
        }
    }).collect();
    
    Ok(Json(rules))
}

pub async fn create_rule(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
    Json(payload): Json<CreateRuleRequest>,
) -> Result<Json<Rule>> {
    let rule_id = Uuid::new_v4().to_string();
    let rule_id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    let action = payload.action.unwrap_or_else(|| "block".to_string());
    
    sqlx::query(
        "INSERT INTO kasno_rules (id, tenant_id, rule_type, pattern, action, source, enabled, hits, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
    )
    .bind(&rule_id)
    .bind(&tenant_id)
    .bind(&payload.rule_type)
    .bind(&payload.pattern)
    .bind(&action)
    .bind("admin")
    .bind(true)
    .bind(0i64)
    .bind(now)
    .bind(now)
    .execute(state.tenant_db.pool())
    .await
    .map_err(|e| AppError::Database(format!("Failed to create rule: {}", e)))?;
    
    Ok(Json(Rule {
        id: rule_id,
        rule_type: payload.rule_type,
        pattern: payload.pattern,
        action,
        source: "admin".to_string(),
        enabled: true,
        hits: 0,
    }))
}

pub async fn update_rule(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
    Path(rule_id): Path<String>,
    Json(payload): Json<UpdateRuleRequest>,
) -> Result<Json<Rule>> {
    let now = Utc::now().timestamp();
    let now = Utc::now().timestamp();
    
    let existing = sqlx::query(
        "SELECT rule_type, pattern, action, enabled, hits FROM kasno_rules WHERE id = $1 AND tenant_id = $2"
    )
    .bind(&rule_id)
    .bind(&tenant_id)
    .fetch_optional(state.tenant_db.pool())
    .await
    .map_err(|e| AppError::Database(format!("Failed to get rule: {}", e)))?;
    
    let existing = existing.ok_or_else(|| AppError::NotFound("Rule not found".to_string()))?;
    
    let rule_type = payload.rule_type.unwrap_or_else(|| existing.get(0));
    let pattern = payload.pattern.unwrap_or_else(|| existing.get(1));
    let action = payload.action.unwrap_or_else(|| existing.get(2));
    let enabled = payload.enabled.unwrap_or(existing.get::<i32, _>(3) != 0);
    let hits: i64 = existing.get(4);
    
    sqlx::query(
        "UPDATE kasno_rules SET rule_type = $1, pattern = $2, action = $3, enabled = $4, updated_at = $5 WHERE id = $6 AND tenant_id = $7"
    )
    .bind(&rule_type)
    .bind(&pattern)
    .bind(&action)
    .bind(enabled)
    .bind(now)
    .bind(&rule_id)
    .bind(&tenant_id)
    .execute(state.tenant_db.pool())
    .await
    .map_err(|e| AppError::Database(format!("Failed to update rule: {}", e)))?;
    
    Ok(Json(Rule {
        id: rule_id,
        rule_type,
        pattern,
        action,
        source: "admin".to_string(),
        enabled,
        hits,
    }))
}

pub async fn delete_rule(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
    Path(rule_id): Path<String>,
) -> Result<StatusCode> {
    
    sqlx::query("DELETE FROM kasno_rules WHERE id = $1 AND tenant_id = $2")
        .bind(&rule_id)
        .bind(&tenant_id)
        .execute(state.tenant_db.pool())
        .await
        .map_err(|e| AppError::Database(format!("Failed to delete rule: {}", e)))?;
    
    Ok(StatusCode::NO_CONTENT)
}

pub async fn register_device(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
    Json(payload): Json<RegisterDeviceRequest>,
) -> Result<Json<RegisterDeviceResponse>> {
    let device_id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    let block_mode = payload.block_mode.unwrap_or_else(|| "aggressive".to_string());
    
    sqlx::query(
        "INSERT INTO kasno_devices (id, tenant_id, name, device_token, status, block_mode, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"
    )
    .bind(&device_id)
    .bind(&tenant_id)
    .bind(&payload.name)
    .bind(&payload.device_token)
    .bind("active")
    .bind(&block_mode)
    .bind(now)
    .bind(now)
    .execute(state.tenant_db.pool())
    .await
    .map_err(|e| AppError::Database(format!("Failed to register device: {}", e)))?;
    
    Ok(Json(RegisterDeviceResponse {
        id: device_id,
        name: payload.name,
        status: "active".to_string(),
        block_mode,
    }))
}

pub async fn list_devices(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
) -> Result<Json<Vec<DeviceStatus>>> {
    
    let rows = sqlx::query(
        "SELECT id, name, status, block_mode, last_seen FROM kasno_devices WHERE tenant_id = $1"
    )
    .bind(&tenant_id)
    .fetch_all(state.tenant_db.pool())
    .await
    .map_err(|e| AppError::Database(format!("Failed to list devices: {}", e)))?;
    
    let devices: Vec<DeviceStatus> = rows.iter().map(|row| {
        DeviceStatus {
            id: row.get(0),
            name: row.get(1),
            status: row.get(2),
            block_mode: row.get(3),
            last_seen: row.get::<Option<i64>, _>(4)
                .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
            vpn_active: false,
            current_risk_score: 0.0,
        }
    }).collect();
    
    Ok(Json(devices))
}

pub async fn get_device(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
    Path(device_id): Path<String>,
) -> Result<Json<DeviceStatus>> {
    
    let row = sqlx::query(
        "SELECT id, name, status, block_mode, last_seen FROM kasno_devices WHERE id = $1 AND tenant_id = $2"
    )
    .bind(&device_id)
    .bind(&tenant_id)
    .fetch_optional(state.tenant_db.pool())
    .await
    .map_err(|e| AppError::Database(format!("Failed to get device: {}", e)))?;
    
    let row = row.ok_or_else(|| AppError::NotFound("Device not found".to_string()))?;
    
    let risk_score: f32 = sqlx::query_scalar(
        "SELECT risk_score FROM kasno_risk_scores WHERE device_id = $1 ORDER BY score_date DESC LIMIT 1"
    )
    .bind(&device_id)
    .fetch_optional(state.tenant_db.pool())
    .await
    .unwrap_or(Some(0.0))
    .unwrap_or(0.0);
    
    Ok(Json(DeviceStatus {
        id: row.get(0),
        name: row.get(1),
        status: row.get(2),
        block_mode: row.get(3),
            last_seen: row.get::<Option<i64>, _>(4)
            .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default(),
        vpn_active: false,
        current_risk_score: risk_score,
    }))
}

#[derive(Debug, Deserialize)]
pub struct UpdateDeviceRequest {
    pub name: Option<String>,
    pub block_mode: Option<String>,
    pub status: Option<String>,
}

pub async fn update_device(
    State(state): State<AppState>,
    TenantId(tenant_id): TenantId,
    Path(device_id): Path<String>,
    Json(payload): Json<UpdateDeviceRequest>,
) -> Result<Json<DeviceStatus>> {
    let now = Utc::now().timestamp();
    
    let existing = sqlx::query(
        "SELECT name, status, block_mode, last_seen FROM kasno_devices WHERE id = $1 AND tenant_id = $2"
    )
    .bind(&device_id)
    .bind(&tenant_id)
    .fetch_optional(state.tenant_db.pool())
    .await
    .map_err(|e| AppError::Database(format!("Failed to get device: {}", e)))?;
    
    let existing = existing.ok_or_else(|| AppError::NotFound("Device not found".to_string()))?;
    
    let name = payload.name.unwrap_or_else(|| existing.get(0));
    let status = payload.status.unwrap_or_else(|| existing.get(1));
    let block_mode = payload.block_mode.unwrap_or_else(|| existing.get(2));
    let last_seen: Option<i64> = existing.get(3);
    
    sqlx::query(
        "UPDATE kasno_devices SET name = $1, status = $2, block_mode = $3, updated_at = $4 WHERE id = $5 AND tenant_id = $6"
    )
    .bind(&name)
    .bind(&status)
    .bind(&block_mode)
    .bind(now)
    .bind(&device_id)
    .bind(&tenant_id)
    .execute(state.tenant_db.pool())
    .await
    .map_err(|e| AppError::Database(format!("Failed to update device: {}", e)))?;
    
    let risk_score: f32 = sqlx::query_scalar(
        "SELECT risk_score FROM kasno_risk_scores WHERE device_id = $1 ORDER BY score_date DESC LIMIT 1"
    )
    .bind(&device_id)
    .fetch_optional(state.tenant_db.pool())
    .await
    .unwrap_or(Some(0.0))
    .unwrap_or(0.0);
    
    Ok(Json(DeviceStatus {
        id: device_id,
        name,
        status,
        block_mode,
        last_seen: last_seen
            .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default(),
        vpn_active: false,
        current_risk_score: risk_score,
    }))
}
