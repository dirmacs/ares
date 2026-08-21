//! Field-change trigger handler.
//!
//! Receives simulated field-change events and executes any matching
//! field-change triggers for the tenant.

use crate::db::schedules as db_schedules;
use crate::trigger_engine;
use ares_types::types::AppError;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;
use std::sync::Arc;
use ares_cordis_core::Context;

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
/// matching agents.  Secured by `X-Webhook-Secret` when `WEBHOOK_SECRET`
/// is configured.
pub async fn handle_field_change(
    State(ctx): State<Arc<Context>>,
    headers: HeaderMap,
    Json(payload): Json<FieldChangeEvent>,
) -> crate::types::Result<StatusCode> {
    verify_webhook_secret(&headers)?;

    if let Some(svc) = ctx.get::<crate::trigger_engine::TriggerService>() {
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

    let __pool_1 = ctx.get::<crate::context_services::TenantDbService>().expect("not provided").0.pool().clone();
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
        if let Err(e) =
            trigger_engine::execute_triggered_agent(&trigger, &message, &app_state).await
        {
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

/// Check the `X-Webhook-Secret` header against the `WEBHOOK_SECRET` env var.
/// If the env var is unset the check is skipped (development mode).
fn verify_webhook_secret(headers: &HeaderMap) -> crate::types::Result<()> {
    let expected = std::env::var("WEBHOOK_SECRET").unwrap_or_default();
    if expected.is_empty() {
        return Ok(());
    }
    let provided = headers
        .get("X-Webhook-Secret")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if provided == expected {
        Ok(())
    } else {
        Err(AppError::Auth("Invalid webhook secret".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::sync::Mutex;

    static WEBHOOK_SECRET_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn verify_webhook_secret_empty_env_allows_all() {
        let _guard = WEBHOOK_SECRET_ENV_LOCK.lock().expect("env lock poisoned");
        std::env::remove_var("WEBHOOK_SECRET");
        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Secret", HeaderValue::from_static("anything"));
        assert!(verify_webhook_secret(&headers).is_ok());
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
}
