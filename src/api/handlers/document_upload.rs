//! Document-upload trigger handler.
//!
//! Receives simulated S3 event notifications and executes any matching
//! document-upload triggers for the tenant.

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

/// Simulated S3 event payload.
#[derive(Debug, Deserialize)]
pub struct DocumentUploadEvent {
    /// Tenant that owns the bucket.
    pub tenant_id: String,
    /// S3 bucket name.
    pub bucket: String,
    /// Object key (path within bucket).
    pub key: String,
    /// Object size in bytes.
    #[serde(default)]
    pub size: i64,
    /// MIME type of the object.
    #[serde(default)]
    pub content_type: String,
    /// Pre-signed URL for fetching the object.
    #[serde(default)]
    pub signed_url: String,
}

/// POST /api/events/document-upload
///
/// Public endpoint that receives document-upload events and triggers
/// matching agents.  Secured by `X-Webhook-Secret` when `WEBHOOK_SECRET`
/// is configured.
pub async fn handle_document_upload(
    State(ctx): State<Arc<Context>>,
    headers: HeaderMap,
    Json(payload): Json<DocumentUploadEvent>,
) -> crate::types::Result<StatusCode> {
    verify_webhook_secret(&headers)?;

    // Prefer TriggerService (Cordis DI) — owns DB + AgentExecutionService.
    // Falls back to direct store + execute_triggered_agent if service absent (tests).
    if let Some(svc) = ctx.get::<crate::trigger_engine::TriggerService>() {
        svc.dispatch_document_upload(
            &payload.tenant_id,
            &payload.bucket,
            &payload.key,
            payload.size,
            &payload.content_type,
            &payload.signed_url,
            &ctx,
        )
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
        return Ok(StatusCode::OK);
    }

    let __pool_1 = ctx.get::<crate::context_services::TenantDbService>().expect("not provided").0.pool().clone();
    let store = db_schedules::EventTriggerStore::new(&__pool_1);
    let triggers = store
        .list_by_event_type(&payload.tenant_id, "document_upload")
        .await?;

    let matching: Vec<_> = triggers
        .into_iter()
        .filter(|trigger| document_upload_trigger_matches(trigger, &payload))
        .collect();

    let app_state = ctx.clone();
    for trigger in matching {
        let context = serde_json::json!({
            "event": "document_upload",
            "bucket": payload.bucket,
            "key": payload.key,
            "size": payload.size,
            "content_type": payload.content_type,
            "signed_url": payload.signed_url,
        });
        let message = serde_json::to_string(&context).unwrap_or_default();
        if let Err(e) =
            trigger_engine::execute_triggered_agent(&trigger, &message, &app_state).await
        {
            tracing::warn!(
                trigger_id = %trigger.id,
                agent = %trigger.target_agent,
                error = %e,
                "Document-upload trigger execution failed"
            );
        }
    }

    Ok(StatusCode::OK)
}

fn document_upload_trigger_matches(
    trigger: &db_schedules::EventTrigger,
    payload: &DocumentUploadEvent,
) -> bool {
    if !trigger.enabled {
        return false;
    }

    let Some(bucket) = trigger.event_config.get("bucket").and_then(|v| v.as_str()) else {
        return false;
    };
    if bucket != payload.bucket {
        return false;
    }

    match trigger.event_config.get("prefix").and_then(|v| v.as_str()) {
        Some(prefix) if !prefix.is_empty() => payload.key.starts_with(prefix),
        _ => true,
    }
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

    fn document_trigger(bucket: &str, prefix: &str, enabled: bool) -> db_schedules::EventTrigger {
        db_schedules::EventTrigger {
            id: "trigger-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            name: "docs".to_string(),
            event_type: "document_upload".to_string(),
            event_config: serde_json::json!({
                "bucket": bucket,
                "prefix": prefix,
            }),
            target_agent: "agent-a".to_string(),
            enabled,
            created_at: 1,
            updated_at: 1,
        }
    }

    fn document_event(bucket: &str, key: &str) -> DocumentUploadEvent {
        DocumentUploadEvent {
            tenant_id: "tenant-1".to_string(),
            bucket: bucket.to_string(),
            key: key.to_string(),
            size: 0,
            content_type: String::new(),
            signed_url: String::new(),
        }
    }

    #[test]
    fn document_upload_trigger_match_honors_optional_prefix() {
        let trigger = document_trigger("docs", "uploads/invoices/", true);

        assert!(document_upload_trigger_matches(
            &trigger,
            &document_event("docs", "uploads/invoices/june.pdf")
        ));
        assert!(!document_upload_trigger_matches(
            &trigger,
            &document_event("docs", "uploads/contracts/june.pdf")
        ));
    }

    #[test]
    fn document_upload_trigger_match_accepts_empty_prefix() {
        let trigger = document_trigger("docs", "", true);

        assert!(document_upload_trigger_matches(
            &trigger,
            &document_event("docs", "any/key.pdf")
        ));
    }

    #[test]
    fn document_upload_trigger_match_rejects_disabled_or_wrong_bucket() {
        assert!(!document_upload_trigger_matches(
            &document_trigger("docs", "", false),
            &document_event("docs", "any/key.pdf")
        ));
        assert!(!document_upload_trigger_matches(
            &document_trigger("docs", "", true),
            &document_event("other", "any/key.pdf")
        ));
    }

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
        std::env::set_var("WEBHOOK_SECRET", "secret123");
        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Secret", HeaderValue::from_static("wrong"));
        assert!(verify_webhook_secret(&headers).is_err());
        std::env::remove_var("WEBHOOK_SECRET");
    }

    #[test]
    fn verify_webhook_secret_accepts_match() {
        let _guard = WEBHOOK_SECRET_ENV_LOCK.lock().expect("env lock poisoned");
        std::env::set_var("WEBHOOK_SECRET", "secret123");
        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Secret", HeaderValue::from_static("secret123"));
        assert!(verify_webhook_secret(&headers).is_ok());
        std::env::remove_var("WEBHOOK_SECRET");
    }
}
