//! POST /v1/usage/events — external usage ingest for library-embedded callers.
//!
//! Turns served outside the HTTP route (a product embedding ARES as a
//! library) never pass `track_usage`, so the embedder reports each turn here
//! after the reply ships. One `usage_events` row per event, tenant-keyed.
//!
//! Contract (generic, not product-specific):
//! - Auth is the standard v1 `Authorization: Bearer ares_...` API-key
//!   middleware. The tenant is derived from the credential; the body carries
//!   no tenant field and cannot attribute to anyone else.
//! - Body is a JSON array of 1..=100 events with `deny_unknown_fields`
//!   (no content fields, ever). Bodies are never logged; only counts.
//! - Per-event idempotency on `(tenant_id, request_id)`; the response is 202
//!   with per-event `recorded` / `deduplicated` status. Semantic validation
//!   is all-or-nothing: a rejected batch can be retried whole because
//!   redelivery deduplicates.
//! - This endpoint is intentionally *unmetered*: reporting usage must not
//!   itself consume quota. Ingested rows carry `source = 'ingest'` and do
//!   not touch the monthly quota cache or daily rate limits.

use super::*;

use std::sync::Arc;

use ares_store::tenant_agents;
use ares_types::models::TenantContext;
use axum::{
    extract::{Extension, State},
    http::{header::CONTENT_LENGTH, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use cordis::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::app_error_into_response;

/// Closed outcome vocabulary for ingested turns.
const OUTCOME_CLASSES: &[&str] = &[
    "ok",
    "client_error",
    "upstream_error",
    "timeout",
    "cancelled",
];
const MAX_EVENTS_PER_POST: usize = 100;
/// A 100-event batch cannot fit a 4 KiB single-event budget (a typical event
/// is ~200 bytes); the cap scales with the batch contract instead. Field
/// allow-lists plus `deny_unknown_fields` carry the no-content guarantee.
const MAX_BODY_BYTES: u64 = 256 * 1024;
const MAX_TOKEN_COUNT: i64 = 10_000_000;
const MAX_LATENCY_MS: i64 = 86_400_000;
const MAX_REASON_CHARS: usize = 128;
const MAX_ID_CHARS: usize = 128;
const OCCURRED_MAX_AGE_SECS: i64 = 30 * 24 * 3600;
const OCCURRED_FUTURE_SKEW_SECS: i64 = 300;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageIngestEvent {
    pub agent: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    pub outcome_class: String,
    #[serde(default)]
    pub reason_code: Option<String>,
    #[serde(default)]
    pub latency_ms: i64,
    pub request_id: String,
    #[serde(default)]
    pub occurred_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct UsageIngestResult {
    pub request_id: String,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UsageIngestResponse {
    pub results: Vec<UsageIngestResult>,
}

fn invalid(message: impl Into<String>) -> Response {
    app_error_into_response(ares_types::AppError::InvalidInput(message.into()))
}

fn check_len(value: &str, field: &str, max_chars: usize, allow_empty: bool) -> Result<(), String> {
    if value.is_empty() && !allow_empty {
        return Err(format!("{field} must not be empty"));
    }
    if value.len() > max_chars || value.chars().count() > max_chars {
        return Err(format!("{field} must be at most {max_chars} chars"));
    }
    Ok(())
}

fn validate_event(ev: &UsageIngestEvent, now: i64) -> Result<(), String> {
    check_len(&ev.agent, "agent", MAX_ID_CHARS, false)?;
    if let Some(model) = &ev.model {
        check_len(model, "model", MAX_ID_CHARS, true)?;
    }
    for (name, value) in [
        ("input_tokens", ev.input_tokens),
        ("output_tokens", ev.output_tokens),
    ] {
        if !(0..=MAX_TOKEN_COUNT).contains(&value) {
            return Err(format!("{name} must be 0..={MAX_TOKEN_COUNT}"));
        }
    }
    if !OUTCOME_CLASSES.contains(&ev.outcome_class.as_str()) {
        return Err(format!(
            "outcome_class must be one of {}",
            OUTCOME_CLASSES.join(", ")
        ));
    }
    if let Some(reason) = &ev.reason_code {
        check_len(reason, "reason_code", MAX_REASON_CHARS, true)?;
    }
    if !(0..=MAX_LATENCY_MS).contains(&ev.latency_ms) {
        return Err(format!("latency_ms must be 0..={MAX_LATENCY_MS}"));
    }
    check_len(&ev.request_id, "request_id", MAX_ID_CHARS, false)?;
    if let Some(ts) = ev.occurred_at {
        if ts < now - OCCURRED_MAX_AGE_SECS || ts > now + OCCURRED_FUTURE_SKEW_SECS {
            return Err("occurred_at is outside the accepted window".to_string());
        }
    }
    Ok(())
}

/// POST /v1/usage/events — record one batch of externally-served turns.
pub async fn ingest_usage_events(
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    headers: HeaderMap,
    Json(events): Json<Vec<UsageIngestEvent>>,
) -> Response {
    if let Some(too_big) = headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(|len| len > MAX_BODY_BYTES)
    {
        if too_big {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({
                    "error": format!("body exceeds {MAX_BODY_BYTES} bytes"),
                    "code": "PAYLOAD_TOO_LARGE",
                })),
            )
                .into_response();
        }
    }

    let tc = match super::shared::extract_tenant(ctx) {
        Ok(tc) => tc,
        Err(e) => return e.into_response(),
    };
    if events.is_empty() || events.len() > MAX_EVENTS_PER_POST {
        return invalid(format!(
            "body must be an array of 1..={MAX_EVENTS_PER_POST} events"
        ));
    }

    let db = state_ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided");
    let pool = db.pool();
    let now = chrono::Utc::now().timestamp();

    for ev in &events {
        if let Err(message) = validate_event(ev, now) {
            return invalid(format!("event '{}': {message}", ev.request_id));
        }
    }

    let mut results = Vec::with_capacity(events.len());
    let mut recorded = 0u64;
    let mut deduplicated = 0u64;
    for ev in &events {
        let agent = match tenant_agents::get_tenant_agent(pool, &tc.tenant_id, &ev.agent).await {
            Ok(agent) => agent,
            Err(ares_types::AppError::NotFound(_)) => {
                return invalid(format!(
                    "event '{}': unknown agent '{}' for this tenant",
                    ev.request_id, ev.agent
                ));
            }
            Err(e) => {
                return app_error_into_response(e);
            }
        };
        if !agent.enabled {
            return invalid(format!(
                "event '{}': agent '{}' is disabled",
                ev.request_id, ev.agent
            ));
        }

        let total_tokens = ev.input_tokens + ev.output_tokens;
        let row: Option<String> = match sqlx::query_scalar(
            "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, tokens_used, effective_tokens, success, duration_ms, created_at, input_tokens, output_tokens, model_name, agent_name, operation, request_id, outcome_class, reason_code)
             VALUES ($1, $2, 'ingest', 1, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'usage.ingest', $13, $14, $15)
             ON CONFLICT (tenant_id, request_id) WHERE request_id IS NOT NULL DO NOTHING RETURNING id",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&tc.tenant_id)
        .bind(total_tokens)
        .bind(total_tokens)
        .bind(total_tokens)
        .bind(ev.outcome_class == "ok")
        .bind(ev.latency_ms)
        .bind(ev.occurred_at.unwrap_or(now))
        .bind(ev.input_tokens)
        .bind(ev.output_tokens)
        .bind(ev.model.as_deref())
        .bind(&ev.agent)
        .bind(&ev.request_id)
        .bind(&ev.outcome_class)
        .bind(ev.reason_code.as_deref())
        .fetch_optional(pool)
        .await
        {
            Ok(row) => row,
            Err(e) => {
                return app_error_into_response(ares_types::AppError::Database(format!(
                    "Failed to record usage event: {e}"
                )));
            }
        };
        match row {
            Some(id) => {
                recorded += 1;
                results.push(UsageIngestResult {
                    request_id: ev.request_id.clone(),
                    status: "recorded",
                    id: Some(id),
                });
            }
            None => {
                deduplicated += 1;
                results.push(UsageIngestResult {
                    request_id: ev.request_id.clone(),
                    status: "deduplicated",
                    id: None,
                });
            }
        }
    }

    tracing::info!(
        tenant_id = %tc.tenant_id,
        events = events.len(),
        recorded,
        deduplicated,
        "usage ingest batch"
    );
    (StatusCode::ACCEPTED, Json(UsageIngestResponse { results })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> UsageIngestEvent {
        UsageIngestEvent {
            agent: "ingest-probe".to_string(),
            model: Some("openai/gpt-oss-20b".to_string()),
            input_tokens: 120,
            output_tokens: 45,
            outcome_class: "ok".to_string(),
            reason_code: None,
            latency_ms: 812,
            request_id: "req-1".to_string(),
            occurred_at: None,
        }
    }

    #[test]
    fn valid_event_passes() {
        assert!(validate_event(&event(), 1_788_442_534).is_ok());
    }

    #[test]
    fn outcome_vocabulary_is_closed() {
        let mut ev = event();
        ev.outcome_class = "maybe".to_string();
        assert!(validate_event(&ev, 1_788_442_534).is_err());
        for ok in [
            "ok",
            "client_error",
            "upstream_error",
            "timeout",
            "cancelled",
        ] {
            ev.outcome_class = ok.to_string();
            assert!(validate_event(&ev, 1_788_442_534).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_empty_request_id_negative_tokens_and_stale_timestamp() {
        let mut ev = event();
        ev.request_id.clear();
        assert!(validate_event(&ev, 1_788_442_534).is_err());
        ev = event();
        ev.input_tokens = -1;
        assert!(validate_event(&ev, 1_788_442_534).is_err());
        ev = event();
        ev.occurred_at = Some(1_788_442_534 - OCCURRED_MAX_AGE_SECS - 1);
        assert!(validate_event(&ev, 1_788_442_534).is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let raw = serde_json::json!([{
            "agent": "ingest-probe",
            "outcome_class": "ok",
            "request_id": "req-1",
            "prompt_text": "must never be accepted",
        }]);
        let parsed: Result<Vec<UsageIngestEvent>, _> = serde_json::from_value(raw);
        assert!(parsed.is_err(), "content fields must not deserialize");
    }
}
