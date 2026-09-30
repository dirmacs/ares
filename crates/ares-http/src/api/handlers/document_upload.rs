//! Document-upload trigger handler.
//!
//! Receives simulated S3 event notifications and executes any matching
//! document-upload triggers for the tenant.

use crate::HttpError;
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
/// matching agents.  Secured by `X-Webhook-Secret`; refuses every request
/// until `WEBHOOK_SECRET` is configured (`verify_webhook_secret`).
pub async fn handle_document_upload(
    State(ctx): State<Arc<Context>>,
    headers: HeaderMap,
    Json(payload): Json<DocumentUploadEvent>,
) -> crate::Result<StatusCode> {
    verify_webhook_secret(&headers)?;

    // Prefer TriggerService (Cordis DI) — owns DB + Execute.
    // Falls back to direct store + execute_triggered_agent if service absent (tests).
    if let Some(svc) = ctx.get::<ares_agent::trigger::TriggerService>() {
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

    let __pool_1 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
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
        if let Err(e) = trigger::execute_triggered_agent(&trigger, &message, &app_state).await {
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
///
/// Fails closed: refuses unless `WEBHOOK_SECRET` is non-empty after trimming
/// **and** the header equals it. Unset, empty and whitespace-only all refuse
/// every request. The one check for both `/api/events/*` routes
/// (`field_change` uses it too).
pub(crate) fn verify_webhook_secret(headers: &HeaderMap) -> crate::Result<()> {
    let expected = std::env::var("WEBHOOK_SECRET")
        .ok()
        .filter(|secret| !secret.trim().is_empty());
    let provided = headers
        .get("X-Webhook-Secret")
        .and_then(|h| h.to_str().ok());
    match (expected, provided) {
        (Some(expected), Some(provided)) if provided == expected => Ok(()),
        _ => Err(HttpError::from(AppError::Auth(
            "Invalid webhook secret".to_string(),
        ))),
    }
}

/// Serializes the unit tests, here and in `field_change`, that mutate the
/// process-global `WEBHOOK_SECRET`: both modules test the one check above.
/// Taken only through [`lock_webhook_secret_env`].
#[cfg(test)]
static WEBHOOK_SECRET_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Takes [`WEBHOOK_SECRET_ENV_LOCK`], recovering it when poisoned: one
/// failing test must not hide another test's own result. Every test sets or
/// removes `WEBHOOK_SECRET` itself before it checks anything.
#[cfg(test)]
pub(crate) fn lock_webhook_secret_env() -> std::sync::MutexGuard<'static, ()> {
    WEBHOOK_SECRET_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Every `X-Webhook-Secret` a caller can send: none, empty, whitespace-only
/// and a non-empty value.
#[cfg(test)]
fn every_webhook_header() -> [(&'static str, HeaderMap); 4] {
    let with = |value: &'static str| {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Webhook-Secret",
            axum::http::HeaderValue::from_static(value),
        );
        headers
    };
    [
        ("missing", HeaderMap::new()),
        ("empty", with("")),
        ("blank", with("   ")),
        ("non-empty", with("anything")),
    ]
}

/// Sets (`Some`) or removes (`None`) `WEBHOOK_SECRET`, runs `check` against
/// [`every_webhook_header`], removes the variable again, and returns the
/// headers `check` let through. Each test module passes its own
/// `verify_webhook_secret`. The caller holds the lock
/// ([`lock_webhook_secret_env`]).
#[cfg(test)]
pub(crate) fn webhook_headers_admitted(
    check: fn(&HeaderMap) -> crate::Result<()>,
    secret: Option<&str>,
) -> Vec<&'static str> {
    match secret {
        Some(value) => std::env::set_var("WEBHOOK_SECRET", value),
        None => std::env::remove_var("WEBHOOK_SECRET"),
    }
    let admitted = every_webhook_header()
        .into_iter()
        .filter(|(_, headers)| check(headers).is_ok())
        .map(|(label, _)| label)
        .collect();
    std::env::remove_var("WEBHOOK_SECRET");
    admitted
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

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
    fn verify_webhook_secret_unset_or_empty_refuses() {
        let _guard = lock_webhook_secret_env();
        let no_header = HeaderMap::new();
        let mut anything = HeaderMap::new();
        anything.insert("X-Webhook-Secret", HeaderValue::from_static("anything"));
        let mut empty = HeaderMap::new();
        empty.insert("X-Webhook-Secret", HeaderValue::from_static(""));
        let mut blank = HeaderMap::new();
        blank.insert("X-Webhook-Secret", HeaderValue::from_static("   "));

        std::env::remove_var("WEBHOOK_SECRET");
        assert!(verify_webhook_secret(&anything).is_err());
        assert!(verify_webhook_secret(&no_header).is_err());
        std::env::set_var("WEBHOOK_SECRET", "");
        assert!(verify_webhook_secret(&no_header).is_err());
        assert!(verify_webhook_secret(&empty).is_err());
        std::env::set_var("WEBHOOK_SECRET", "   ");
        assert!(verify_webhook_secret(&blank).is_err());
        std::env::remove_var("WEBHOOK_SECRET");
    }

    #[test]
    fn verify_webhook_secret_rejects_mismatch() {
        let _guard = lock_webhook_secret_env();
        std::env::set_var("WEBHOOK_SECRET", "secret123");
        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Secret", HeaderValue::from_static("wrong"));
        assert!(verify_webhook_secret(&headers).is_err());
        std::env::remove_var("WEBHOOK_SECRET");
    }

    #[test]
    fn verify_webhook_secret_accepts_match() {
        let _guard = lock_webhook_secret_env();
        std::env::set_var("WEBHOOK_SECRET", "secret123");
        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Secret", HeaderValue::from_static("secret123"));
        assert!(verify_webhook_secret(&headers).is_ok());
        std::env::remove_var("WEBHOOK_SECRET");
    }

    /// Probe E7: `WEBHOOK_SECRET` set empty skipped the check. Every header
    /// is now refused.
    #[test]
    fn verify_webhook_secret_e7_empty_secret_refuses_every_header() {
        let _guard = lock_webhook_secret_env();
        let admitted = webhook_headers_admitted(verify_webhook_secret, Some(""));
        assert!(admitted.is_empty(), "E7: let through {admitted:?}");
    }

    /// Unset refuses every request (the amending ruling §2.1).
    #[test]
    fn verify_webhook_secret_unset_refuses_every_header() {
        let _guard = lock_webhook_secret_env();
        let admitted = webhook_headers_admitted(verify_webhook_secret, None);
        assert!(admitted.is_empty(), "unset: let through {admitted:?}");
    }

    /// Whitespace-only is treated as unset: not even the same whitespace
    /// matches.
    #[test]
    fn verify_webhook_secret_blank_secret_refuses_every_header() {
        let _guard = lock_webhook_secret_env();
        let admitted = webhook_headers_admitted(verify_webhook_secret, Some("   "));
        assert!(admitted.is_empty(), "blank: let through {admitted:?}");
    }
}
