//! Document-upload trigger handler, and the one webhook-secret check the three
//! public webhook routes share.
//!
//! Receives simulated S3 event notifications and executes any matching
//! document-upload triggers for the tenant.
//!
//! # Webhook authentication (item 2.16a)
//!
//! `POST /api/events/document-upload`, `POST /api/events/field-change` and
//! `POST /api/webhooks/{trigger_id}` are public routes with one mechanism: the
//! caller presents its **tenant's own event secret** in `X-Webhook-Secret`
//! ([`authenticate_webhook`]). The secret is stored only as its SHA-256
//! (`tenant_event_secrets`, migration 041), and **the tenant is the tenant of
//! the secret that matched**: a request body never names the tenant on its own
//! authority.
//!
//! - no secret, an empty or blank one, or one no tenant has: **401**, one body
//!   for every cause (the 2.16e body);
//! - a valid secret whose tenant is not the one the request names (a body
//!   `tenant_id` that differs, or a trigger of another tenant): **403**;
//! - a database failure while looking the secret up: the request is refused
//!   with the error's own status (500), never admitted.
//!
//! The process-wide `WEBHOOK_SECRET` variable is not read: it is not a
//! credential any more. Neither a secret nor its hash is ever logged.

use crate::api::handlers::admin::shared::constant_time_eq;
use crate::HttpError;
use ares_agent::trigger;
use ares_store::schedules as db_schedules;
use ares_types::types::{AppError, ErrorCode};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use cordis::Context;
use serde::Deserialize;
use std::sync::Arc;

/// The header that carries a tenant's event secret.
const WEBHOOK_SECRET_HEADER: &str = "X-Webhook-Secret";

/// Why a webhook route did not run a trigger.
#[derive(Debug)]
pub enum WebhookError {
    /// 401: no secret, an empty or blank one, or one no tenant has. One body
    /// for every cause: the caller learns nothing about which it was.
    Unauthorized,
    /// 403: the secret is a tenant's, but not valid for what the request names.
    Forbidden(Forbidden),
    /// Anything else (a failed lookup, a failed dispatch), answered by its
    /// [`AppError`]'s own status and body.
    Failed(HttpError),
}

/// What a valid secret was refused for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forbidden {
    /// The body's `tenant_id` is not the secret's tenant.
    BodyTenantMismatch,
    /// The trigger belongs to another tenant than the secret's.
    TriggerOfAnotherTenant,
}

impl Forbidden {
    fn message(self) -> &'static str {
        match self {
            Forbidden::BodyTenantMismatch => {
                "webhook secret is not valid for the tenant named in the request"
            }
            Forbidden::TriggerOfAnotherTenant => "webhook secret is not valid for this trigger",
        }
    }
}

impl IntoResponse for WebhookError {
    fn into_response(self) -> Response {
        match self {
            // The 2.16e refusal, byte for byte.
            WebhookError::Unauthorized => {
                HttpError::from(AppError::Auth("Invalid webhook secret".to_string()))
                    .into_response()
            }
            WebhookError::Forbidden(reason) => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": format!("Authorization error: {}", reason.message()),
                    "code": ErrorCode::AuthorizationFailed,
                })),
            )
                .into_response(),
            WebhookError::Failed(err) => err.into_response(),
        }
    }
}

impl From<HttpError> for WebhookError {
    fn from(err: HttpError) -> Self {
        WebhookError::Failed(err)
    }
}

impl From<AppError> for WebhookError {
    fn from(err: AppError) -> Self {
        WebhookError::Failed(HttpError::from(err))
    }
}

/// The tenant whose event secret the request presents, or why it has none.
///
/// Reads `X-Webhook-Secret`; a missing header, one that is not visible ASCII,
/// an empty or blank one and one no tenant has are all
/// [`WebhookError::Unauthorized`], and the first three never reach the
/// database. The lookup hashes the secret, finds the row and confirms the two
/// hashes with the platform's constant-time comparison
/// (`ares_store::tenant_event_secrets::tenant_for_secret`). A missing store or a
/// failed lookup is an error, never an admission.
///
/// The one check for all three webhook routes. Nothing here logs the secret or
/// its hash.
pub(crate) async fn authenticate_webhook(
    ctx: &Arc<Context>,
    headers: &HeaderMap,
) -> Result<String, WebhookError> {
    let Some(presented) = headers
        .get(WEBHOOK_SECRET_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(WebhookError::Unauthorized);
    };
    if presented.trim().is_empty() {
        return Err(WebhookError::Unauthorized);
    }
    let Some(tenant_db) = ctx.get::<ares_store::TenantDb>() else {
        return Err(WebhookError::Failed(HttpError::from(
            AppError::Configuration("tenant store is not available".to_string()),
        )));
    };
    match ares_store::tenant_event_secrets::tenant_for_secret(
        tenant_db.pool(),
        presented,
        constant_time_eq,
    )
    .await
    {
        Ok(Some(tenant_id)) => Ok(tenant_id),
        Ok(None) => Err(WebhookError::Unauthorized),
        Err(err) => Err(WebhookError::from(err)),
    }
}

/// A body `tenant_id`, when present, must be the secret's tenant. Absent is
/// fine: the tenant comes from the secret either way.
pub(crate) fn require_body_tenant(
    body_tenant: Option<&str>,
    secret_tenant: &str,
) -> Result<(), WebhookError> {
    match body_tenant {
        Some(named) if named != secret_tenant => {
            Err(WebhookError::Forbidden(Forbidden::BodyTenantMismatch))
        }
        _ => Ok(()),
    }
}

/// Simulated S3 event payload.
#[derive(Debug, Deserialize)]
pub struct DocumentUploadEvent {
    /// Tenant that owns the bucket. Optional: the tenant is the one whose
    /// secret the request presents. When present it must equal that tenant, or
    /// the request is refused with 403.
    #[serde(default)]
    pub tenant_id: Option<String>,
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
/// matching agents of the tenant whose secret the request presents.  Secured
/// by that tenant's own `X-Webhook-Secret` ([`authenticate_webhook`]); a body
/// `tenant_id` that is not that tenant is refused.
pub async fn handle_document_upload(
    State(ctx): State<Arc<Context>>,
    headers: HeaderMap,
    Json(payload): Json<DocumentUploadEvent>,
) -> Result<StatusCode, WebhookError> {
    let tenant_id = authenticate_webhook(&ctx, &headers).await?;
    require_body_tenant(payload.tenant_id.as_deref(), &tenant_id)?;

    // Prefer TriggerService (Cordis DI) — owns DB + Execute.
    // Falls back to direct store + execute_triggered_agent if service absent (tests).
    if let Some(svc) = ctx.get::<ares_agent::trigger::TriggerService>() {
        svc.dispatch_document_upload(
            &tenant_id,
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
        .list_by_event_type(&tenant_id, "document_upload")
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

/// Serializes the unit tests, here and in `field_change`, that mutate the
/// process-global `WEBHOOK_SECRET`: they show that the old variable is not a
/// credential. Taken only through [`lock_webhook_secret_env`].
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

/// Every `X-Webhook-Secret` that cannot be a tenant's secret and must never
/// reach the database: none, empty, whitespace-only and not visible ASCII.
#[cfg(test)]
pub(crate) fn headers_without_a_usable_secret() -> [(&'static str, HeaderMap); 4] {
    let with = |value: axum::http::HeaderValue| {
        let mut headers = HeaderMap::new();
        headers.insert(WEBHOOK_SECRET_HEADER, value);
        headers
    };
    [
        ("missing", HeaderMap::new()),
        ("empty", with(axum::http::HeaderValue::from_static(""))),
        ("blank", with(axum::http::HeaderValue::from_static("   "))),
        (
            "not visible ASCII",
            with(axum::http::HeaderValue::from_bytes(b"\xff\xfe").expect("opaque bytes")),
        ),
    ]
}

/// The response a refusal turns into: status and parsed body.
#[cfg(test)]
pub(crate) async fn refusal_parts(err: WebhookError) -> (StatusCode, serde_json::Value) {
    let response = err.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).expect("a refusal body is JSON"),
    )
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
            tenant_id: Some("tenant-1".to_string()),
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

    /// Replaces the 2.16e `verify_webhook_secret_unset_or_empty_refuses`,
    /// `..._e7_empty_secret_refuses_every_header`, `..._unset_refuses_every_header`
    /// and `..._blank_secret_refuses_every_header` on the check that replaced
    /// `verify_webhook_secret`: with no usable secret in the header the request
    /// is refused 401, and the store is never reached (the context has none, so
    /// a lookup would be a 500), whatever the old variable holds.
    // Deliberate: the lock spans the awaited check, so no other test changes the
    // process-global `WEBHOOK_SECRET` while this one holds it in a given state.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn authenticate_webhook_refuses_a_header_without_a_usable_secret() {
        let _guard = lock_webhook_secret_env();
        let ctx = Context::new_root();
        for env in [None, Some(""), Some("   "), Some("anything")] {
            match env {
                Some(value) => std::env::set_var("WEBHOOK_SECRET", value),
                None => std::env::remove_var("WEBHOOK_SECRET"),
            }
            for (label, headers) in headers_without_a_usable_secret() {
                let err = authenticate_webhook(&ctx, &headers)
                    .await
                    .expect_err("no usable secret must be refused");
                let (status, body) = refusal_parts(err).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{label}, env {env:?}");
                assert_eq!(
                    body,
                    serde_json::json!({
                        "error": "Authentication error: Invalid webhook secret",
                        "code": "AUTHENTICATION_FAILED",
                    }),
                    "{label}, env {env:?}"
                );
            }
        }
        std::env::remove_var("WEBHOOK_SECRET");
    }

    /// Replaces the 2.16e `verify_webhook_secret_accepts_match`: the old
    /// variable matching the header admits nothing. With a secret in the header
    /// and no store, the answer is a 500 (fail closed), never an admission.
    // Deliberate: as above, the lock spans the awaited check.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn authenticate_webhook_ignores_the_old_environment_variable() {
        let _guard = lock_webhook_secret_env();
        let ctx = Context::new_root();
        std::env::set_var("WEBHOOK_SECRET", "secret123");
        let mut headers = HeaderMap::new();
        headers.insert(WEBHOOK_SECRET_HEADER, HeaderValue::from_static("secret123"));
        let err = authenticate_webhook(&ctx, &headers)
            .await
            .expect_err("the old variable is not a credential");
        std::env::remove_var("WEBHOOK_SECRET");
        let (status, _) = refusal_parts(err).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// The header is read case-insensitively, like any HTTP header: a secret
    /// under `x-webhook-secret` gets past the empty-header refusal and reaches
    /// the store step (here a 500, because this context has no store).
    #[tokio::test]
    async fn authenticate_webhook_reads_the_header_case_insensitively() {
        let ctx = Context::new_root();
        for name in ["x-webhook-secret", "X-WEBHOOK-SECRET", "X-Webhook-Secret"] {
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
                HeaderValue::from_static("a-secret-dummy-2-16a"),
            );
            let err = authenticate_webhook(&ctx, &headers)
                .await
                .expect_err("no store on this context");
            let (status, _) = refusal_parts(err).await;
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{name}");
        }
    }

    #[test]
    fn a_null_body_tenant_id_is_absent() {
        let event: DocumentUploadEvent = serde_json::from_value(
            serde_json::json!({"tenant_id": null, "bucket": "b", "key": "k"}),
        )
        .expect("a null tenant_id parses");
        assert_eq!(event.tenant_id, None);
    }

    #[test]
    fn a_body_tenant_id_that_is_not_a_string_does_not_parse() {
        let parsed = serde_json::from_value::<DocumentUploadEvent>(
            serde_json::json!({"tenant_id": 7, "bucket": "b", "key": "k"}),
        );
        assert!(parsed.is_err());
    }

    #[test]
    fn a_body_tenant_that_is_absent_or_equal_passes() {
        assert!(require_body_tenant(None, "tenant-a").is_ok());
        assert!(require_body_tenant(Some("tenant-a"), "tenant-a").is_ok());
    }

    #[test]
    fn a_body_tenant_that_differs_in_any_way_is_forbidden() {
        for named in ["tenant-b", "", " tenant-a", "tenant-a ", "TENANT-A"] {
            assert!(
                matches!(
                    require_body_tenant(Some(named), "tenant-a"),
                    Err(WebhookError::Forbidden(Forbidden::BodyTenantMismatch))
                ),
                "{named:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_two_forbidden_reasons_are_403_with_the_authorization_code() {
        for reason in [
            Forbidden::BodyTenantMismatch,
            Forbidden::TriggerOfAnotherTenant,
        ] {
            let (status, body) = refusal_parts(WebhookError::Forbidden(reason)).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(body["code"], "AUTHORIZATION_FAILED");
            let text = body["error"].as_str().expect("error text");
            assert!(text.starts_with("Authorization error: "), "{text}");
        }
    }

    #[test]
    fn the_event_body_tenant_id_is_optional() {
        let without: DocumentUploadEvent =
            serde_json::from_value(serde_json::json!({"bucket": "b", "key": "k"}))
                .expect("a body without tenant_id parses");
        assert_eq!(without.tenant_id, None);
        let with: DocumentUploadEvent = serde_json::from_value(
            serde_json::json!({"tenant_id": "t", "bucket": "b", "key": "k"}),
        )
        .expect("a body with tenant_id parses");
        assert_eq!(with.tenant_id.as_deref(), Some("t"));
    }
}
