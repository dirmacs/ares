// cordis Phase6: decomposed into v1/{chat,stream,agents} — 3 modules
// shim retains original handlers; new domain modules live in src/api/handlers/v1/*.rs
//! V1 API handlers — tenant-scoped endpoints authenticated via API key.
//!
//! These endpoints are called by enterprise-portal and other client apps
//! using `Authorization: Bearer ares_xxx`. The `api_key_auth_middleware`
//! injects `TenantContext` into request extensions before these handlers run.

use crate::agents::context_provider::AgentRuntimeContext;
use crate::agents::tenant_agent;
use crate::db::agent_runs;
use crate::db::run_history::{LogToolCallRequest, RunHistoryStore};
use crate::db::tenant_agents::{self, TenantAgent};
use crate::memory::estimate_tokens;
use crate::models::{TenantContext, TenantTier};
use crate::observability::RunObservability;
use crate::research::coordinator::ResearchCoordinator;
use crate::types::{
    AgentContext, AgentType, AppError, ChatRequest, ChatResponse, ResearchRequest,
    ResearchResponse, Result,
};
use crate::AppState;
use ares_agents::Agent;
use ares_types::types::ToolDefinition;
use axum::{
    extract::{Extension, Path, Query, State},
    http::{HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// cordis Phase6: decomposed v1 submodules — each provides `pub fn routes() -> RouteSet` stub.
#[path = "v1/chat.rs"]
pub mod chat;
#[path = "v1/stream.rs"]
pub mod stream;
#[path = "v1/agents.rs"]
pub mod agents;

// cordis Phase6: bodies moved to v1/*, shim retained for one release
#[allow(deprecated)] pub use self::chat::*;
#[allow(deprecated)] pub use self::stream::*;
#[allow(deprecated)] pub use self::agents::*;

// =============================================================================
// Response types — designed to match enterprise-portal's expected types
// =============================================================================

#[derive(Debug, Serialize)]
pub struct V1Agent {
    pub id: String,
    pub name: String,
    pub agent_type: String,
    pub status: V1AgentStatus,
    pub config: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub total_runs: u64,
    pub success_rate: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum V1AgentStatus {
    Active,
    Idle,
    Error,
    Disabled,
}

impl From<TenantAgent> for V1Agent {
    fn from(a: TenantAgent) -> Self {
        let status = if a.enabled {
            V1AgentStatus::Active
        } else {
            V1AgentStatus::Disabled
        };
        Self {
            id: a.id,
            name: a.agent_name,
            agent_type: "custom".to_string(),
            status,
            config: a.config,
            created_at: ts_to_dt(a.created_at),
            last_run: None,
            total_runs: 0,
            success_rate: 0.0,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct V1AgentRun {
    pub id: String,
    pub agent_id: String,
    pub status: String,
    pub input: serde_json::Value,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub tokens_used: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct V1AgentLog {
    pub id: String,
    pub agent_id: String,
    pub run_id: Option<String>,
    pub level: String,
    pub message: String,
    pub metadata: Option<serde_json::Value>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct Paginated<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub page: u32,
    pub per_page: u32,
    pub total_pages: u32,
}

impl<T> Paginated<T> {
    fn empty(page: u32, per_page: u32) -> Self {
        Self {
            items: vec![],
            total: 0,
            page,
            per_page,
            total_pages: 0,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct V1Usage {
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub total_runs: u64,
    pub total_tokens: u64,
    pub total_api_calls: u64,
    pub quota_runs: Option<u64>,
    pub quota_tokens: Option<u64>,
    pub daily_usage: Vec<DailyUsage>,
}

#[derive(Debug, Serialize)]
pub struct DailyUsage {
    pub date: String,
    pub runs: u64,
    pub tokens: u64,
    pub api_calls: u64,
}

#[derive(Debug, Serialize)]
pub struct V1ApiKey {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub created_at: DateTime<Utc>,
    pub last_used: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    pub expires_in_days: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct CreateApiKeyResponse {
    pub key: V1ApiKey,
    pub secret: String,
}

#[derive(Debug, Deserialize)]
pub struct PaginationQuery {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

// =============================================================================
// Helpers
// =============================================================================

pub(crate) fn ts_to_dt(ts: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now)
}

pub(crate) fn extract_tenant(ctx: Option<Extension<TenantContext>>) -> Result<TenantContext> {
    ctx.map(|Extension(c)| c)
        .ok_or_else(|| AppError::Auth("Missing tenant context".to_string()))
}

pub(crate) fn set_header(headers: &mut axum::http::HeaderMap, name: &'static str, value: impl ToString) {
    if let Ok(value) = HeaderValue::from_str(&value.to_string()) {
        headers.insert(HeaderName::from_static(name), value);
    }
}

pub(crate) fn normalize_page(page: Option<u32>) -> u32 {
    page.unwrap_or(1).max(1)
}

pub(crate) fn normalize_per_page(per_page: Option<u32>, default: u32) -> u32 {
    per_page.unwrap_or(default).min(100)
}

pub(crate) fn logs_pagination(page: Option<u32>, per_page: Option<u32>) -> (u32, u32) {
    (page.unwrap_or(1), per_page.unwrap_or(50))
}

pub(crate) fn compute_total_pages(total: u64, per_page: u32) -> u32 {
    if per_page == 0 {
        return 0;
    }
    ((total as f64) / (per_page as f64)).ceil() as u32
}

pub(crate) fn paginate_vec<T>(items: Vec<T>, page: u32, per_page: u32) -> Paginated<T> {
    let total = items.len() as u64;
    let total_pages = compute_total_pages(total, per_page);
    let start = ((page.saturating_sub(1)) * per_page) as usize;
    let page_items = items
        .into_iter()
        .skip(start)
        .take(per_page as usize)
        .collect();
    Paginated {
        items: page_items,
        total,
        page,
        per_page,
        total_pages,
    }
}

pub(crate) fn list_runs_offset(page: u32, per_page: u32) -> i64 {
    ((page - 1) * per_page) as i64
}

pub(crate) fn quota_display_limit(limit: u64) -> Option<u64> {
    if limit == u64::MAX {
        None
    } else {
        Some(limit)
    }
}

pub(crate) fn check_tenant_request_quota(
    tc: &TenantContext,
    monthly_requests: u64,
    daily_requests: u64,
) -> Result<()> {
    if tc.tier == TenantTier::Enterprise {
        return Ok(());
    }
    if tc.can_make_request(monthly_requests, daily_requests) {
        Ok(())
    } else {
        Err(AppError::RateLimited(format!(
            "Quota exceeded for {:?} tier. Monthly: {}/{}, Daily: {}/{}",
            tc.tier,
            monthly_requests,
            tc.quota.requests_per_month,
            daily_requests,
            tc.quota.requests_per_day
        )))
    }
}

pub(crate) fn research_depth_and_iterations(
    payload_depth: Option<u8>,
    payload_max_iterations: Option<u8>,
    workflow_depth: Option<u8>,
    workflow_max_iterations: Option<u8>,
) -> (u8, u8) {
    (
        payload_depth.unwrap_or_else(|| workflow_depth.unwrap_or(2)),
        payload_max_iterations.unwrap_or_else(|| workflow_max_iterations.unwrap_or(5)),
    )
}

pub(crate) fn extract_agent_run_message(input: &serde_json::Value) -> String {
    input
        .get("message")
        .or_else(|| input.get("input"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| serde_json::to_string(input).unwrap_or_default())
}

pub(crate) fn extract_workspace_id(input: &serde_json::Value) -> Option<String> {
    input
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub(crate) fn format_message_with_context(context: &str, message: &str) -> String {
    format!("{}\n\n---\nUser message: {}", context, message)
}

pub(crate) fn llm_token_counts_u64(
    usage: Option<&ares_llm::client::TokenUsage>,
    input_fallback: &str,
    output_fallback: &str,
) -> (u64, u64) {
    if let Some(u) = usage {
        (u.prompt_tokens as u64, u.completion_tokens as u64)
    } else {
        (
            estimate_tokens(input_fallback) as u64,
            estimate_tokens(output_fallback) as u64,
        )
    }
}

pub(crate) fn llm_token_counts_u32(
    usage: Option<&ares_llm::client::TokenUsage>,
    input_fallback: &str,
    output_fallback: &str,
) -> (u32, u32) {
    let (input, output) = llm_token_counts_u64(usage, input_fallback, output_fallback);
    (input as u32, output as u32)
}

pub(crate) fn execution_metadata_names(
    metadata: Option<&crate::agents::ExecutionMetadata>,
) -> (String, String) {
    metadata
        .map(|m| (m.model_name.clone(), m.provider_name.clone()))
        .unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()))
}

pub(crate) fn agent_run_row_to_v1(r: agent_runs::AgentRun) -> V1AgentRun {
    V1AgentRun {
        id: r.id,
        agent_id: r.agent_name,
        status: r.status,
        input: serde_json::json!({"tokens": r.input_tokens}),
        output: Some(serde_json::json!({"tokens": r.output_tokens})),
        error: r.error,
        started_at: ts_to_dt(r.created_at),
        finished_at: Some(ts_to_dt(r.created_at + (r.duration_ms / 1000))),
        duration_ms: Some(r.duration_ms as u64),
        tokens_used: Some((r.input_tokens + r.output_tokens) as u64),
    }
}

pub(crate) fn usage_period_start(now: DateTime<Utc>) -> DateTime<Utc> {
    now.date_naive()
        .with_day(1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
}

pub(crate) fn usage_response<T: Serialize>(
    payload: T,
    input_tokens: u64,
    output_tokens: u64,
    model_name: &str,
    provider_name: &str,
    agent_name: &str,
) -> Response {
    let mut response = Json(payload).into_response();
    let headers = response.headers_mut();
    set_header(headers, "x-input-tokens", input_tokens);
    set_header(headers, "x-output-tokens", output_tokens);
    set_header(headers, "x-model-name", model_name);
    set_header(headers, "x-provider-name", provider_name);
    set_header(headers, "x-agent-name", agent_name);
    response
}

pub(crate) async fn enforce_quota(state: &AppState, tc: &TenantContext) -> Result<()> {
    if tc.tier == TenantTier::Enterprise {
        return Ok(());
    }
    let monthly = state
        .tenant_db
        .get_monthly_requests(&tc.tenant_id)
        .await
        .unwrap_or(0);
    let daily = state
        .tenant_db
        .get_daily_requests(&tc.tenant_id)
        .await
        .unwrap_or(0);
    check_tenant_request_quota(tc, monthly, daily)
}

// =============================================================================
// Handlers
// =============================================================================















#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use chrono::TimeZone;

    fn sample_tenant_agent(enabled: bool) -> TenantAgent {
        TenantAgent {
            id: "agent-row-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            agent_name: "support-bot".to_string(),
            display_name: "Support Bot".to_string(),
            description: Some("handles tickets".to_string()),
            config: serde_json::json!({"model": "fast"}),
            enabled,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_100,
        }
    }

    #[test]
    fn ts_to_dt_converts_valid_unix_timestamp() {
        let dt = ts_to_dt(1_700_000_000);
        assert_eq!(dt, Utc.timestamp_opt(1_700_000_000, 0).single().unwrap());
    }

    #[test]
    fn ts_to_dt_invalid_timestamp_falls_back_to_now() {
        let before = Utc::now();
        let dt = ts_to_dt(i64::MAX);
        let after = Utc::now();
        assert!(dt >= before && dt <= after);
    }

    #[test]
    fn extract_tenant_returns_context_when_present() {
        let ctx = TenantContext::new("tenant-42".into(), TenantTier::Pro);
        let got = extract_tenant(Some(Extension(ctx.clone()))).expect("tenant context");
        assert_eq!(got.tenant_id, ctx.tenant_id);
        assert_eq!(got.tier, ctx.tier);
    }

    #[test]
    fn extract_tenant_missing_context_is_auth_error() {
        let err = extract_tenant(None).unwrap_err();
        match err {
            AppError::Auth(msg) => assert!(msg.contains("Missing tenant context")),
            other => panic!("expected Auth error, got {other:?}"),
        }
    }

    #[test]
    fn set_header_inserts_valid_values() {
        let mut headers = axum::http::HeaderMap::new();
        set_header(&mut headers, "x-model-name", "gpt-test");
        assert_eq!(
            headers.get("x-model-name").and_then(|v| v.to_str().ok()),
            Some("gpt-test")
        );
    }

    #[test]
    fn set_header_skips_invalid_header_values() {
        let mut headers = axum::http::HeaderMap::new();
        set_header(&mut headers, "x-model-name", "\ninvalid");
        assert!(headers.get("x-model-name").is_none());
    }

    #[tokio::test]
    async fn usage_response_sets_metering_headers_and_json_body() {
        let response = usage_response(
            serde_json::json!({"answer": "ok"}),
            12,
            34,
            "gpt-test",
            "openai",
            "router",
        );

        assert_eq!(
            response
                .headers()
                .get("x-input-tokens")
                .and_then(|v| v.to_str().ok()),
            Some("12")
        );
        assert_eq!(
            response
                .headers()
                .get("x-output-tokens")
                .and_then(|v| v.to_str().ok()),
            Some("34")
        );
        assert_eq!(
            response
                .headers()
                .get("x-model-name")
                .and_then(|v| v.to_str().ok()),
            Some("gpt-test")
        );
        assert_eq!(
            response
                .headers()
                .get("x-provider-name")
                .and_then(|v| v.to_str().ok()),
            Some("openai")
        );
        assert_eq!(
            response
                .headers()
                .get("x-agent-name")
                .and_then(|v| v.to_str().ok()),
            Some("router")
        );

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(json["answer"], "ok");
    }

    #[test]
    fn tenant_agent_to_v1_agent_maps_enabled_status() {
        let v1: V1Agent = sample_tenant_agent(true).into();
        assert_eq!(v1.id, "agent-row-1");
        assert_eq!(v1.name, "support-bot");
        assert_eq!(v1.agent_type, "custom");
        assert!(matches!(v1.status, V1AgentStatus::Active));
        assert_eq!(v1.config, serde_json::json!({"model": "fast"}));
        assert_eq!(
            v1.created_at,
            Utc.timestamp_opt(1_700_000_000, 0).single().unwrap()
        );
        assert!(v1.last_run.is_none());
        assert_eq!(v1.total_runs, 0);
        assert_eq!(v1.success_rate, 0.0);
    }

    #[test]
    fn tenant_agent_to_v1_agent_maps_disabled_status() {
        let v1: V1Agent = sample_tenant_agent(false).into();
        assert!(matches!(v1.status, V1AgentStatus::Disabled));
    }

    #[test]
    fn paginated_empty_serializes_zero_totals() {
        let page = Paginated::<V1Agent>::empty(2, 25);
        assert!(page.items.is_empty());
        assert_eq!(page.total, 0);
        assert_eq!(page.page, 2);
        assert_eq!(page.per_page, 25);
        assert_eq!(page.total_pages, 0);

        let json = serde_json::to_value(&page).expect("serialize paginated");
        assert_eq!(json["items"], serde_json::json!([]));
        assert_eq!(json["total"], 0);
        assert_eq!(json["page"], 2);
        assert_eq!(json["per_page"], 25);
        assert_eq!(json["total_pages"], 0);
    }

    #[test]
    fn v1_agent_status_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&V1AgentStatus::Active).unwrap(),
            "\"active\""
        );
        assert_eq!(
            serde_json::to_string(&V1AgentStatus::Disabled).unwrap(),
            "\"disabled\""
        );
    }

    #[test]
    fn pagination_query_deserializes_optional_fields() {
        let q: PaginationQuery =
            serde_json::from_str(r#"{"page":3,"per_page":50}"#).expect("deserialize");
        assert_eq!(q.page, Some(3));
        assert_eq!(q.per_page, Some(50));

        let defaults: PaginationQuery = serde_json::from_str("{}").expect("empty object");
        assert!(defaults.page.is_none());
        assert!(defaults.per_page.is_none());
    }

    #[test]
    fn create_api_key_request_deserializes_expiry() {
        let req: CreateApiKeyRequest =
            serde_json::from_str(r#"{"name":"ci-key","expires_in_days":30}"#).expect("deserialize");
        assert_eq!(req.name, "ci-key");
        assert_eq!(req.expires_in_days, Some(30));
    }

    #[test]
    fn v1_usage_round_trips_through_json() {
        let usage = V1Usage {
            period_start: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            period_end: Utc.with_ymd_and_hms(2024, 1, 31, 23, 59, 59).unwrap(),
            total_runs: 10,
            total_tokens: 5000,
            total_api_calls: 42,
            quota_runs: Some(1000),
            quota_tokens: Some(1_000_000),
            daily_usage: vec![DailyUsage {
                date: "2024-01-15".into(),
                runs: 2,
                tokens: 800,
                api_calls: 5,
            }],
        };

        let json = serde_json::to_value(&usage).expect("serialize");
        assert_eq!(json["total_runs"], 10);
        assert_eq!(json["daily_usage"][0]["date"], "2024-01-15");
    }
    #[test]
    fn v1_agent_run_serializes_optional_fields() {
        let started = Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0).unwrap();
        let run = V1AgentRun {
            id: "run-1".into(),
            agent_id: "agent-1".into(),
            status: "completed".into(),
            input: serde_json::json!({"prompt": "hi"}),
            output: Some(serde_json::json!({"text": "hello"})),
            error: None,
            started_at: started,
            finished_at: Some(started + chrono::Duration::seconds(2)),
            duration_ms: Some(2000),
            tokens_used: Some(42),
        };

        let json = serde_json::to_value(&run).expect("serialize run");
        assert_eq!(json["status"], "completed");
        assert_eq!(json["duration_ms"], 2000);
        assert!(json["error"].is_null());
    }

    #[test]
    fn v1_agent_log_serializes_metadata() {
        let log = V1AgentLog {
            id: "log-1".into(),
            agent_id: "agent-1".into(),
            run_id: Some("run-1".into()),
            level: "info".into(),
            message: "started".into(),
            metadata: Some(serde_json::json!({"step": 1})),
            timestamp: Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 1).unwrap(),
        };

        let json = serde_json::to_value(&log).expect("serialize log");
        assert_eq!(json["level"], "info");
        assert_eq!(json["metadata"]["step"], 1);
    }

    #[test]
    fn v1_api_key_and_create_response_round_trip() {
        let created = Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap();
        let key = V1ApiKey {
            id: "key-1".into(),
            name: "ci".into(),
            prefix: "ares_ab".into(),
            created_at: created,
            last_used: None,
            expires_at: Some(created + chrono::Duration::days(30)),
        };
        let response = CreateApiKeyResponse {
            key,
            secret: "ares_secret_value".into(),
        };

        let json = serde_json::to_value(&response).expect("serialize response");
        assert_eq!(json["secret"], "ares_secret_value");
        assert_eq!(json["key"]["prefix"], "ares_ab");
        assert!(json["key"]["last_used"].is_null());
    }

    #[test]
    fn v1_agent_status_idle_and_error_serialize() {
        assert_eq!(
            serde_json::to_string(&V1AgentStatus::Idle).unwrap(),
            "\"idle\""
        );
        assert_eq!(
            serde_json::to_string(&V1AgentStatus::Error).unwrap(),
            "\"error\""
        );
    }

    #[test]
    fn v1_agent_serializes_snake_case_fields() {
        let agent = V1Agent {
            id: "agent-1".into(),
            name: "support".into(),
            agent_type: "custom".into(),
            status: V1AgentStatus::Active,
            config: serde_json::json!({"temperature": 0.2}),
            created_at: Utc.with_ymd_and_hms(2024, 5, 1, 0, 0, 0).unwrap(),
            last_run: None,
            total_runs: 7,
            success_rate: 0.85,
        };

        let json = serde_json::to_value(&agent).expect("serialize agent");
        assert_eq!(json["agent_type"], "custom");
        assert_eq!(json["total_runs"], 7);
        assert_eq!(json["success_rate"], 0.85);
        assert!(json["last_run"].is_null());
    }

    #[test]
    fn daily_usage_serializes_snake_case_fields() {
        let daily = DailyUsage {
            date: "2024-05-01".into(),
            runs: 3,
            tokens: 1200,
            api_calls: 9,
        };
        let json = serde_json::to_value(&daily).expect("serialize daily usage");
        assert_eq!(json["api_calls"], 9);
        assert_eq!(json["tokens"], 1200);
    }

    #[test]
    fn normalize_page_defaults_and_clamps_zero() {
        assert_eq!(normalize_page(None), 1);
        assert_eq!(normalize_page(Some(0)), 1);
        assert_eq!(normalize_page(Some(3)), 3);
    }

    #[test]
    fn normalize_per_page_defaults_and_caps() {
        assert_eq!(normalize_per_page(None, 20), 20);
        assert_eq!(normalize_per_page(Some(500), 20), 100);
        assert_eq!(normalize_per_page(Some(10), 25), 10);
    }

    #[test]
    fn logs_pagination_uses_defaults() {
        assert_eq!(logs_pagination(None, None), (1, 50));
        assert_eq!(logs_pagination(Some(2), Some(10)), (2, 10));
    }

    #[test]
    fn compute_total_pages_rounds_up() {
        assert_eq!(compute_total_pages(0, 20), 0);
        assert_eq!(compute_total_pages(1, 20), 1);
        assert_eq!(compute_total_pages(21, 20), 2);
        assert_eq!(compute_total_pages(40, 20), 2);
        assert_eq!(compute_total_pages(41, 20), 3);
    }

    #[test]
    fn paginate_vec_slices_items() {
        let items: Vec<u32> = (1..=5).collect();
        let page = paginate_vec(items, 2, 2);
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.total, 5);
        assert_eq!(page.page, 2);
        assert_eq!(page.per_page, 2);
        assert_eq!(page.total_pages, 3);
    }

    #[test]
    fn paginate_vec_serializes_full_page() {
        let page = paginate_vec(vec!["a", "b"], 1, 10);
        let json = serde_json::to_value(&page).expect("serialize");
        assert_eq!(json["items"], serde_json::json!(["a", "b"]));
        assert_eq!(json["total"], 2);
        assert_eq!(json["total_pages"], 1);
    }

    #[test]
    fn list_runs_offset_for_page_two() {
        assert_eq!(list_runs_offset(1, 25), 0);
        assert_eq!(list_runs_offset(2, 25), 25);
        assert_eq!(list_runs_offset(3, 10), 20);
    }

    #[test]
    fn quota_display_limit_hides_unlimited() {
        assert_eq!(quota_display_limit(u64::MAX), None);
        assert_eq!(quota_display_limit(1_000), Some(1_000));
    }

    #[test]
    fn check_tenant_request_quota_allows_enterprise() {
        let tc = TenantContext::new("ent".into(), TenantTier::Enterprise);
        check_tenant_request_quota(&tc, u64::MAX, u64::MAX).expect("enterprise bypass");
    }

    #[test]
    fn check_tenant_request_quota_rejects_over_limit() {
        let tc = TenantContext::new("free".into(), TenantTier::Free);
        let err = check_tenant_request_quota(&tc, 1_000, 0).unwrap_err();
        match err {
            AppError::RateLimited(msg) => {
                assert!(msg.contains("Quota exceeded"));
                assert!(msg.contains("Monthly: 1000"));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn check_tenant_request_quota_allows_under_limit() {
        let tc = TenantContext::new("free".into(), TenantTier::Free);
        check_tenant_request_quota(&tc, 0, 0).expect("under quota");
    }

    #[test]
    fn research_model_allowlist_decision_rejects_disallowed_model() {
        let err = research_model_allowlist_decision(false, "gpt-test").unwrap_err();
        match err {
            AppError::Auth(msg) => assert!(msg.contains("gpt-test")),
            other => panic!("expected Auth error, got {other:?}"),
        }
    }

    #[test]
    fn research_model_allowlist_decision_allows_allowed_model() {
        assert!(research_model_allowlist_decision(true, "gpt-test").is_ok());
    }

    #[test]
    fn research_depth_and_iterations_prefers_payload() {
        assert_eq!(
            research_depth_and_iterations(Some(4), Some(8), Some(2), Some(5)),
            (4, 8)
        );
    }

    #[test]
    fn research_depth_and_iterations_uses_workflow_then_defaults() {
        assert_eq!(
            research_depth_and_iterations(None, None, Some(3), Some(7)),
            (3, 7)
        );
        assert_eq!(
            research_depth_and_iterations(None, None, None, None),
            (2, 5)
        );
    }

    #[test]
    fn extract_agent_run_message_prefers_message_field() {
        let input = serde_json::json!({"message": "hello"});
        assert_eq!(extract_agent_run_message(&input), "hello");
    }

    #[test]
    fn extract_agent_run_message_falls_back_to_input_field() {
        let input = serde_json::json!({"input": "from-input"});
        assert_eq!(extract_agent_run_message(&input), "from-input");
    }

    #[test]
    fn extract_agent_run_message_serializes_object_without_text_fields() {
        let input = serde_json::json!({"count": 3});
        assert_eq!(extract_agent_run_message(&input), r#"{"count":3}"#);
    }

    #[test]
    fn sandbox_tool_call_requests_persist_skipped_trace_rows() {
        let tool_specs = vec![
            SandboxToolTraceSpec {
                name: "slack_send_message".to_string(),
                tool_type: "mcp".to_string(),
            },
            SandboxToolTraceSpec {
                name: "tenant_http_tool".to_string(),
                tool_type: "http".to_string(),
            },
            SandboxToolTraceSpec {
                name: "tenant_sql_tool".to_string(),
                tool_type: "sql".to_string(),
            },
        ];
        let calls = sandbox_tool_call_requests("run-1", "tenant-1", "agent-1", &tool_specs, 123);
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].run_id, "run-1");
        assert_eq!(calls[0].tenant_id, "tenant-1");
        assert_eq!(calls[0].agent_name, "agent-1");
        assert_eq!(calls[0].step_index, 0);
        assert_eq!(calls[0].tool_name, "slack_send_message");
        assert_eq!(calls[0].tool_type, "mcp");
        assert_eq!(calls[0].status, "success");
        assert_eq!(calls[0].latency_ms, 0);
        assert_eq!(calls[0].created_at, 123);
        assert_eq!(
            calls[0].result,
            Some(serde_json::json!({"status":"skipped","reason":"sandbox_mode"}))
        );
        assert_eq!(calls[1].step_index, 1);
        assert_eq!(calls[1].tool_name, "tenant_http_tool");
        assert_eq!(calls[1].tool_type, "http");
        assert_eq!(calls[2].step_index, 2);
        assert_eq!(calls[2].tool_name, "tenant_sql_tool");
        assert_eq!(calls[2].tool_type, "sql");
    }

    #[test]
    fn extract_workspace_id_reads_string_field() {
        let input = serde_json::json!({"workspace_id": "ws-9"});
        assert_eq!(extract_workspace_id(&input), Some("ws-9".into()));
        assert_eq!(extract_workspace_id(&serde_json::json!({})), None);
    }

    #[test]
    fn format_message_with_context_joins_blocks() {
        let out = format_message_with_context("ctx", "hi");
        assert!(out.contains("ctx"));
        assert!(out.contains("User message: hi"));
        assert!(out.contains("---"));
    }

    #[test]
    fn llm_token_counts_use_usage_when_present() {
        use ares_llm::client::TokenUsage;
        let usage = TokenUsage {
            prompt_tokens: 11,
            completion_tokens: 22,
            total_tokens: 33,
        };
        let (input, output) = llm_token_counts_u64(Some(&usage), "ignored", "ignored");
        assert_eq!((input, output), (11, 22));
    }

    #[test]
    fn llm_token_counts_estimate_when_usage_missing() {
        let (input, output) = llm_token_counts_u32(None, "hello world", "bye");
        assert!(input > 0);
        assert!(output > 0);
    }

    #[test]
    fn execution_metadata_names_default_unknown() {
        let (model, provider) = execution_metadata_names(None);
        assert_eq!(model, "unknown");
        assert_eq!(provider, "unknown");
    }

    #[test]
    fn execution_metadata_names_reads_metadata() {
        use crate::agents::ExecutionMetadata;
        let meta = ExecutionMetadata {
            model_name: "gpt-test".into(),
            provider_name: "openai".into(),
        };
        let (model, provider) = execution_metadata_names(Some(&meta));
        assert_eq!(model, "gpt-test");
        assert_eq!(provider, "openai");
    }

    #[test]
    fn agent_run_row_to_v1_maps_db_row() {
        let row = agent_runs::AgentRun {
            id: "run-1".into(),
            tenant_id: "t1".into(),
            agent_name: "bot".into(),
            user_id: None,
            workspace_id: None,
            session_id: None,
            status: "completed".into(),
            input_tokens: 10,
            output_tokens: 20,
            duration_ms: 1500,
            error: None,
            created_at: 1_700_000_000,
            model_name: "m".into(),
            provider_name: "p".into(),
            is_streaming: false,
            request_source: None,
            product: None,
            agent_config_source: None,
            agent_config_version: None,
            eruka_binding_id: None,
            eruka_context_hit: false,
            eruka_read_count: 0,
            eruka_write_count: 0,
            pipeline_id: None,
            schedule_id: None,
            trigger_id: None,
        };
        let v1 = agent_run_row_to_v1(row);
        assert_eq!(v1.id, "run-1");
        assert_eq!(v1.agent_id, "bot");
        assert_eq!(v1.tokens_used, Some(30));
        assert_eq!(v1.duration_ms, Some(1500));
    }

    #[test]
    fn usage_period_start_is_first_day_of_month() {
        let now = Utc.with_ymd_and_hms(2024, 6, 15, 12, 30, 0).unwrap();
        let start = usage_period_start(now);
        assert_eq!(start, Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn create_api_key_request_deserializes_without_expiry() {
        let req: CreateApiKeyRequest =
            serde_json::from_str(r#"{"name":"no-expiry"}"#).expect("deserialize");
        assert_eq!(req.name, "no-expiry");
        assert!(req.expires_in_days.is_none());
    }

    #[test]
    fn v1_usage_serializes_without_quota_caps() {
        let usage = V1Usage {
            period_start: Utc.with_ymd_and_hms(2024, 2, 1, 0, 0, 0).unwrap(),
            period_end: Utc.with_ymd_and_hms(2024, 2, 29, 0, 0, 0).unwrap(),
            total_runs: 1,
            total_tokens: 2,
            total_api_calls: 3,
            quota_runs: None,
            quota_tokens: None,
            daily_usage: vec![],
        };
        let json = serde_json::to_value(&usage).expect("serialize");
        assert!(json["quota_runs"].is_null());
        assert!(json["quota_tokens"].is_null());
    }

    #[test]
    fn v1_agent_run_serializes_error_field() {
        let run = V1AgentRun {
            id: "run-err".into(),
            agent_id: "agent".into(),
            status: "failed".into(),
            input: serde_json::json!({}),
            output: None,
            error: Some("boom".into()),
            started_at: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            finished_at: None,
            duration_ms: None,
            tokens_used: None,
        };
        let json = serde_json::to_value(&run).expect("serialize");
        assert_eq!(json["error"], "boom");
        assert!(json["output"].is_null());
    }

    #[test]
    fn v1_agent_log_serializes_without_optionals() {
        let log = V1AgentLog {
            id: "log-2".into(),
            agent_id: "agent".into(),
            run_id: None,
            level: "warn".into(),
            message: "slow".into(),
            metadata: None,
            timestamp: Utc.with_ymd_and_hms(2024, 1, 2, 0, 0, 0).unwrap(),
        };
        let json = serde_json::to_value(&log).expect("serialize");
        assert!(json["run_id"].is_null());
        assert!(json["metadata"].is_null());
    }

    #[test]
    fn ts_to_dt_unix_epoch() {
        let dt = ts_to_dt(0);
        assert_eq!(dt, Utc.timestamp_opt(0, 0).single().unwrap());
    }
}