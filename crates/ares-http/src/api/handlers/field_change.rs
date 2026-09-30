//! Field-change trigger handler.
//!
//! Receives simulated field-change events and executes any matching
//! field-change triggers for the tenant.

use crate::api::handlers::document_upload::verify_webhook_secret;
use ares_agent::trigger;
use ares_store::schedules as db_schedules;
use ares_types::types::AppError;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use cordis::Context;
use serde::Deserialize;
use std::sync::Arc;

/// Simulated database field-change payload.
#[derive(Debug, Deserialize)]
pub struct FieldChangeEvent {
    /// Tenant that owns the record.
    pub tenant_id: String,
    /// Table name where the change occurred.
    pub table: String,
    /// Column name that changed.
    pub column: String,
    /// Primary key / record identifier.
    pub record_id: String,
    /// Previous value (JSON to accommodate any type).
    pub old_value: serde_json::Value,
    /// New value (JSON to accommodate any type).
    pub new_value: serde_json::Value,
}

/// POST /api/events/field-change
///
/// Public endpoint that receives field-change events and triggers
/// matching agents.  Secured by `X-Webhook-Secret`; refuses every request
/// until `WEBHOOK_SECRET` is configured (`verify_webhook_secret`).
pub async fn handle_field_change(
    State(ctx): State<Arc<Context>>,
    headers: HeaderMap,
    Json(payload): Json<FieldChangeEvent>,
) -> crate::Result<StatusCode> {
    verify_webhook_secret(&headers)?;

    if let Some(svc) = ctx.get::<ares_agent::trigger::TriggerService>() {
        svc.dispatch_field_change(
            &payload.tenant_id,
            &payload.table,
            &payload.column,
            &payload.record_id,
            payload.old_value.clone(),
            payload.new_value.clone(),
            &ctx,
        )
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
        return Ok(StatusCode::OK);
    }

    let __pool_1 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let store = db_schedules::EventTriggerStore::new(&__pool_1);
    let triggers = store
        .list_by_event_type(&payload.tenant_id, "field_change")
        .await?;

    let matching: Vec<_> = triggers
        .into_iter()
        .filter(|t| t.enabled)
        .filter(|t| {
            let table_match = t
                .event_config
                .get("table")
                .and_then(|v| v.as_str())
                .map(|tbl| tbl == payload.table)
                .unwrap_or(false);
            let column_match = t
                .event_config
                .get("column")
                .and_then(|v| v.as_str())
                .map(|col| col == payload.column)
                .unwrap_or(false);
            table_match && column_match
        })
        .collect();

    let app_state = ctx.clone();
    for trigger in matching {
        let context = serde_json::json!({
            "event": "field_change",
            "table": payload.table,
            "column": payload.column,
            "record_id": payload.record_id,
            "old_value": payload.old_value,
            "new_value": payload.new_value,
        });
        let message = serde_json::to_string(&context).unwrap_or_default();
        if let Err(e) = trigger::execute_triggered_agent(&trigger, &message, &app_state).await {
            tracing::warn!(
                trigger_id = %trigger.id,
                agent = %trigger.target_agent,
                error = %e,
                "Field-change trigger execution failed"
            );
        }
    }

    Ok(StatusCode::OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::handlers::document_upload::{
        webhook_headers_admitted, WEBHOOK_SECRET_ENV_LOCK,
    };
    use axum::http::HeaderValue;

    #[test]
    fn verify_webhook_secret_unset_or_empty_refuses() {
        let _guard = WEBHOOK_SECRET_ENV_LOCK.lock().expect("env lock poisoned");
        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Secret", HeaderValue::from_static("anything"));
        std::env::remove_var("WEBHOOK_SECRET");
        assert!(verify_webhook_secret(&headers).is_err());
        std::env::set_var("WEBHOOK_SECRET", "");
        assert!(verify_webhook_secret(&HeaderMap::new()).is_err());
        std::env::remove_var("WEBHOOK_SECRET");
    }

    #[test]
    fn verify_webhook_secret_rejects_mismatch() {
        let _guard = WEBHOOK_SECRET_ENV_LOCK.lock().expect("env lock poisoned");
        std::env::set_var("WEBHOOK_SECRET", "secret456");
        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Secret", HeaderValue::from_static("wrong"));
        assert!(verify_webhook_secret(&headers).is_err());
        std::env::remove_var("WEBHOOK_SECRET");
    }

    #[test]
    fn verify_webhook_secret_accepts_match() {
        let _guard = WEBHOOK_SECRET_ENV_LOCK.lock().expect("env lock poisoned");
        std::env::set_var("WEBHOOK_SECRET", "secret456");
        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Secret", HeaderValue::from_static("secret456"));
        assert!(verify_webhook_secret(&headers).is_ok());
        std::env::remove_var("WEBHOOK_SECRET");
    }

    /// Probe E7 on this route's check: `WEBHOOK_SECRET` set empty refuses
    /// every header.
    #[test]
    fn verify_webhook_secret_e7_empty_secret_refuses_every_header() {
        let _guard = WEBHOOK_SECRET_ENV_LOCK.lock().expect("env lock poisoned");
        let admitted = webhook_headers_admitted(verify_webhook_secret, Some(""));
        assert!(admitted.is_empty(), "E7: let through {admitted:?}");
    }

    /// Unset refuses every request (the amending ruling §2.1).
    #[test]
    fn verify_webhook_secret_unset_refuses_every_header() {
        let _guard = WEBHOOK_SECRET_ENV_LOCK.lock().expect("env lock poisoned");
        let admitted = webhook_headers_admitted(verify_webhook_secret, None);
        assert!(admitted.is_empty(), "unset: let through {admitted:?}");
    }

    /// Whitespace-only is treated as unset: not even the same whitespace
    /// matches.
    #[test]
    fn verify_webhook_secret_blank_secret_refuses_every_header() {
        let _guard = WEBHOOK_SECRET_ENV_LOCK.lock().expect("env lock poisoned");
        let admitted = webhook_headers_admitted(verify_webhook_secret, Some("   "));
        assert!(admitted.is_empty(), "blank: let through {admitted:?}");
    }
}
