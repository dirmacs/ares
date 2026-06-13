//! Document-upload trigger handler.
//!
//! Receives simulated S3 event notifications and executes any matching
//! document-upload triggers for the tenant.

use crate::db::schedules as db_schedules;
use crate::{AppState, trigger_engine};
use ares_types::types::AppError;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;
use std::sync::Arc;

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
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<DocumentUploadEvent>,
) -> crate::types::Result<StatusCode> {
    verify_webhook_secret(&headers)?;

    let store = db_schedules::EventTriggerStore::new(state.tenant_db.pool());
    let triggers = store
        .list_by_event_type(&payload.tenant_id, "document_upload")
        .await?;

    let matching: Vec<_> = triggers
        .into_iter()
        .filter(|t| t.enabled)
        .filter(|t| {
            t.event_config
                .get("bucket")
                .and_then(|v| v.as_str())
                .map(|b| b == payload.bucket)
                .unwrap_or(false)
        })
        .collect();

    let app_state = Arc::new(state);
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
        if let Err(e) = trigger_engine::execute_triggered_agent(
            &trigger,
            &message,
            &app_state,
        )
        .await
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

    #[test]
    fn verify_webhook_secret_empty_env_allows_all() {
        std::env::remove_var("WEBHOOK_SECRET");
        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Secret", HeaderValue::from_static("anything"));
        assert!(verify_webhook_secret(&headers).is_ok());
    }

    #[test]
    fn verify_webhook_secret_rejects_mismatch() {
        std::env::set_var("WEBHOOK_SECRET", "secret123");
        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Secret", HeaderValue::from_static("wrong"));
        assert!(verify_webhook_secret(&headers).is_err());
        std::env::remove_var("WEBHOOK_SECRET");
    }

    #[test]
    fn verify_webhook_secret_accepts_match() {
        std::env::set_var("WEBHOOK_SECRET", "secret123");
        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Secret", HeaderValue::from_static("secret123"));
        assert!(verify_webhook_secret(&headers).is_ok());
        std::env::remove_var("WEBHOOK_SECRET");
    }
}
