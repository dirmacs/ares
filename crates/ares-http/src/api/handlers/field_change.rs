//! Field-change trigger handler.
//!
//! Receives simulated field-change events and executes any matching
//! field-change triggers for the tenant. The tenant is the one whose secret the
//! request presents (`document_upload::authenticate_webhook`), never the body's.

use crate::api::handlers::document_upload::{
    authenticate_webhook, require_body_tenant, WebhookError,
};
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
    /// Tenant that owns the record. Optional: the tenant is the one whose
    /// secret the request presents. When present it must equal that tenant, or
    /// the request is refused with 403.
    #[serde(default)]
    pub tenant_id: Option<String>,
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
/// matching agents of the tenant whose secret the request presents.  Secured
/// by that tenant's own `X-Webhook-Secret` (`authenticate_webhook`); a body
/// `tenant_id` that is not that tenant is refused.
pub async fn handle_field_change(
    State(ctx): State<Arc<Context>>,
    headers: HeaderMap,
    Json(payload): Json<FieldChangeEvent>,
) -> Result<StatusCode, WebhookError> {
    let tenant_id = authenticate_webhook(&ctx, &headers).await?;
    require_body_tenant(payload.tenant_id.as_deref(), &tenant_id)?;

    if let Some(svc) = ctx.get::<ares_agent::trigger::TriggerService>() {
        svc.dispatch_field_change(
            &tenant_id,
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
    let triggers = store.list_by_event_type(&tenant_id, "field_change").await?;

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
        headers_without_a_usable_secret, lock_webhook_secret_env, refusal_parts,
    };
    use axum::http::HeaderValue;

    /// Replaces the 2.16e `verify_webhook_secret_unset_or_empty_refuses`,
    /// `..._e7_empty_secret_refuses_every_header`, `..._unset_refuses_every_header`
    /// and `..._blank_secret_refuses_every_header` for this route, which shares
    /// the check with `document_upload`: no usable secret in the header is a 401
    /// that never reaches the store (this context has none), whatever the old
    /// variable holds.
    // Deliberate: the lock spans the awaited check, so no other test changes the
    // process-global `WEBHOOK_SECRET` while this one holds it in a given state.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn field_change_refuses_a_header_without_a_usable_secret() {
        let _guard = lock_webhook_secret_env();
        let ctx = Context::new_root();
        for env in [None, Some(""), Some("   "), Some("anything")] {
            match env {
                Some(value) => std::env::set_var("WEBHOOK_SECRET", value),
                None => std::env::remove_var("WEBHOOK_SECRET"),
            }
            for (label, headers) in headers_without_a_usable_secret() {
                let payload = FieldChangeEvent {
                    tenant_id: Some("tenant-a".to_string()),
                    table: "t".to_string(),
                    column: "c".to_string(),
                    record_id: "r".to_string(),
                    old_value: serde_json::json!(1),
                    new_value: serde_json::json!(2),
                };
                let err = handle_field_change(State(ctx.clone()), headers, Json(payload))
                    .await
                    .expect_err("no usable secret must be refused");
                let (status, _) = refusal_parts(err).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{label}, env {env:?}");
            }
        }
        std::env::remove_var("WEBHOOK_SECRET");
    }

    /// Replaces the 2.16e `verify_webhook_secret_accepts_match` for this route:
    /// the old variable matching the header admits nothing (no store: 500, fail
    /// closed).
    // Deliberate: as above, the lock spans the awaited check.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn field_change_ignores_the_old_environment_variable() {
        let _guard = lock_webhook_secret_env();
        let ctx = Context::new_root();
        std::env::set_var("WEBHOOK_SECRET", "secret456");
        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Secret", HeaderValue::from_static("secret456"));
        let payload = FieldChangeEvent {
            tenant_id: None,
            table: "t".to_string(),
            column: "c".to_string(),
            record_id: "r".to_string(),
            old_value: serde_json::json!(1),
            new_value: serde_json::json!(2),
        };
        let result = handle_field_change(State(ctx), headers, Json(payload)).await;
        std::env::remove_var("WEBHOOK_SECRET");
        let (status, _) = refusal_parts(result.expect_err("not admitted")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn the_event_body_tenant_id_is_optional() {
        let body = |tenant: Option<&str>| {
            let mut value = serde_json::json!({
                "table": "t", "column": "c", "record_id": "r", "old_value": 1, "new_value": 2,
            });
            if let Some(tenant) = tenant {
                value["tenant_id"] = serde_json::json!(tenant);
            }
            value
        };
        let without: FieldChangeEvent =
            serde_json::from_value(body(None)).expect("a body without tenant_id parses");
        assert_eq!(without.tenant_id, None);
        let with: FieldChangeEvent =
            serde_json::from_value(body(Some("t1"))).expect("a body with tenant_id parses");
        assert_eq!(with.tenant_id.as_deref(), Some("t1"));
    }

    #[test]
    fn a_null_body_tenant_id_is_absent() {
        let event: FieldChangeEvent = serde_json::from_value(serde_json::json!({
            "tenant_id": null, "table": "t", "column": "c", "record_id": "r",
            "old_value": 1, "new_value": 2,
        }))
        .expect("a null tenant_id parses");
        assert_eq!(event.tenant_id, None);
    }

    #[test]
    fn a_body_tenant_that_differs_is_refused_before_any_trigger_is_looked_up() {
        assert!(matches!(
            require_body_tenant(Some("tenant-b"), "tenant-a"),
            Err(WebhookError::Forbidden(_))
        ));
        assert!(require_body_tenant(Some("tenant-a"), "tenant-a").is_ok());
        assert!(require_body_tenant(None, "tenant-a").is_ok());
    }
}
