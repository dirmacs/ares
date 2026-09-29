//! Live-DB tests for item 1.16 (VERIFY-2026-09-22.md §4 row 20): every named admin/key-lifecycle
//! write must leave exactly one `admin_audit_log` row, with its actor,
//! landed BEFORE the handler's response is returned (no sleep, no polling) —
//! and a failed audit insert must be visible at `error` level, never dropped.
//!
//! Requires a live Postgres reachable via `TEST_DATABASE_URL` (never
//! `ares_test`; see the brief). With the variable set and the database
//! unreachable a test **panics** (naming the variable, never its value): a
//! configured run does not skip. With it unset the crate's skip convention
//! applies (`tests/common/mod.rs`). Handlers are called directly as plain async
//! functions with hand-built extractors (`State`, `Extension`, `Path`,
//! `Json`) — the same in-process pattern `ares-http`'s own
//! `middleware/api_key_auth.rs` test module uses for its live-DB cases —
//! which lets each test assert on the database in the same task, right after
//! the `.await` that produced the response, with nothing in between.
//!
//! `#[cfg(feature = "postgres")]`, not `#[ignore]`: these are meant to run in
//! the normal `cargo test --locked` sweep (they are the brief's failing/
//! passing evidence). Unconfigured, they skip cleanly, matching
//! `live_chat_stream.rs`; configured, they never skip.

#![cfg(feature = "postgres")]

mod common;

use std::sync::{Arc, Mutex};

use ares_http::api::handlers::admin::billing::set_token_budget;
use ares_http::api::handlers::admin::cordis::provide_cordis_service;
use ares_http::api::handlers::admin::providers::{
    delete_runtime_provider, upsert_runtime_provider,
};
use ares_http::api::handlers::admin::shared::{
    CreateRuntimeProviderRequest, RuntimeProviderScopeQuery, SetTokenBudgetRequest,
};
use ares_http::api::handlers::admin::{update_tenant_agent_handler, AdminActor};
use ares_http::api::handlers::v1::{
    create_api_key, delete_tenant_data, revoke_api_key, rotate_api_key, CreateApiKeyRequest,
    RotateApiKeyRequest,
};
use ares_store::tenant_agents::{create_tenant_agent, CreateTenantAgentRequest};
use ares_store::TenantDb;
use ares_types::models::{TenantContext, TenantTier};
use ares_types::types::AppError;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use cordis::Context;
use sqlx::PgPool;
use sqlx::Row;

/// A fresh root `Context` carrying a real, connected `TenantDb`, plus the
/// pool underneath it for the test's own assertions. `None` only for the
/// unconfigured skip; a configured run with no database panics in
/// `common::live_db_url`.
async fn live_ctx() -> Option<(Arc<Context>, PgPool)> {
    common::live_db_url(&common::current_test_name()).await?;
    let pg = ares_test_support::client().await;
    let pool = pg.pool.clone();
    let tenant_db = Arc::new(TenantDb::new(Arc::new(pg)));
    let ctx = Context::new_root();
    ctx.provide_arc(tenant_db);
    Some((ctx, pool))
}

async fn admin_audit_row_count(
    pool: &PgPool,
    action: &str,
    resource_type: &str,
    resource_id: &str,
) -> (i64, Option<String>) {
    let row = sqlx::query(
        "SELECT COUNT(*) AS c, MAX(actor) AS actor FROM admin_audit_log \
         WHERE action = $1 AND resource_type = $2 AND resource_id = $3",
    )
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .fetch_one(pool)
    .await
    .expect("count query");
    (
        row.get::<i64, _>("c"),
        row.get::<Option<String>, _>("actor"),
    )
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

// ---------------------------------------------------------------------------
// v1 key lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn audit_row_lands_before_response_v1_key_create() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant_db = ctx.get::<TenantDb>().expect("TenantDb");
    let tenant = tenant_db
        .create_tenant(unique("t1116-create"), TenantTier::Free)
        .await
        .expect("create tenant");
    let tc = TenantContext::new(tenant.id.clone(), TenantTier::Free);

    let resp = create_api_key(
        State(ctx.clone()),
        Some(Extension(tc)),
        None,
        HeaderMap::new(),
        Json(CreateApiKeyRequest {
            name: "audit-test-key".to_string(),
            expires_in_days: None,
            scopes: None,
        }),
    )
    .await
    .expect("create_api_key response");
    let key_id = resp.0.key.id.clone();

    // Right after the response, no sleep: the row must already be there.
    let (count, actor) = admin_audit_row_count(&pool, "create_api_key", "api_key", &key_id).await;
    assert_eq!(
        count, 1,
        "expected exactly one admin_audit_log row for create_api_key/{key_id} immediately after the response"
    );
    assert_eq!(
        actor.as_deref(),
        Some(tenant.id.as_str()),
        "actor must be the tenant id for a v1 tenant-surface mint"
    );
}

#[tokio::test]
async fn audit_row_lands_before_response_v1_key_revoke() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant_db = ctx.get::<TenantDb>().expect("TenantDb");
    let tenant = tenant_db
        .create_tenant(unique("t1116-revoke"), TenantTier::Free)
        .await
        .expect("create tenant");
    let (api_key, _raw) = tenant_db
        .create_api_key(&tenant.id, "to-revoke".to_string(), None, None)
        .await
        .expect("create api key directly");
    let tc = TenantContext::new(tenant.id.clone(), TenantTier::Free);

    let status = revoke_api_key(
        State(ctx.clone()),
        Some(Extension(tc)),
        None,
        Path(api_key.id.clone()),
        HeaderMap::new(),
    )
    .await
    .expect("revoke_api_key response");
    assert_eq!(status, axum::http::StatusCode::NO_CONTENT);

    let (count, actor) =
        admin_audit_row_count(&pool, "revoke_api_key", "api_key", &api_key.id).await;
    assert_eq!(
        count, 1,
        "expected exactly one admin_audit_log row for revoke_api_key/{} immediately after the response",
        api_key.id
    );
    assert_eq!(actor.as_deref(), Some(tenant.id.as_str()));
}

// ---------------------------------------------------------------------------
// Admin tenant-agent update (the dashboard's agent-config save)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn audit_row_lands_before_response_admin_tenant_agent_update() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant_db = ctx.get::<TenantDb>().expect("TenantDb");
    let tenant_id = unique("t1116-agent-tenant");
    let agent_name = unique("audit-test-agent");

    let created = create_tenant_agent(
        pool_ref(&tenant_db),
        &tenant_id,
        CreateTenantAgentRequest {
            agent_name: agent_name.clone(),
            display_name: "Before".to_string(),
            description: None,
            config: serde_json::json!({"model": "audit-test-model"}),
        },
    )
    .await
    .expect("seed tenant agent");

    let actor = AdminActor {
        subject: Some("audit-test-admin".to_string()),
        email: None,
        auth: Some("jwt"),
        client_ip: Some("203.0.113.99".to_string()),
    };

    let updated = update_tenant_agent_handler(
        State(ctx.clone()),
        Path((tenant_id.clone(), agent_name.clone())),
        actor,
        Json(ares_store::tenant_agents::UpdateTenantAgentRequest {
            display_name: Some("After".to_string()),
            description: None,
            config: None,
            enabled: None,
        }),
    )
    .await
    .expect("update_tenant_agent_handler response");
    assert_eq!(updated.0.id, created.id);
    assert_eq!(updated.0.display_name, "After");

    let (count, actor_col) =
        admin_audit_row_count(&pool, "update_agent", "agent", &created.id).await;
    assert_eq!(
        count, 1,
        "expected exactly one admin_audit_log row for update_agent/{} immediately after the response",
        created.id
    );
    assert_eq!(actor_col.as_deref(), Some("audit-test-admin"));
}

fn pool_ref(tenant_db: &Arc<TenantDb>) -> &PgPool {
    tenant_db.pool()
}

// ---------------------------------------------------------------------------
// Admin runtime-provider create / delete (row 66: had zero audit calls)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn audit_row_lands_before_response_admin_runtime_provider_create() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let name = unique("audit-test-provider");

    let resp = upsert_runtime_provider(
        State(ctx.clone()),
        AdminActor {
            subject: Some("audit-test-admin".to_string()),
            email: None,
            auth: Some("jwt"),
            client_ip: Some("203.0.113.99".to_string()),
        },
        Json(CreateRuntimeProviderRequest {
            tenant_id: None,
            name: name.clone(),
            display_name: "Audit Test Provider".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://example.test/v1".to_string(),
            auth_type: "api_key".to_string(),
            default_model: None,
            headers: None,
            request_transform: None,
            response_transform: None,
            enabled: Some(true),
        }),
    )
    .await
    .expect("upsert_runtime_provider response");
    assert_eq!(resp.0.name, name);

    let (count, actor) =
        admin_audit_row_count(&pool, "create_runtime_provider", "runtime_provider", &name).await;
    assert_eq!(
        count, 1,
        "expected exactly one admin_audit_log row for create_runtime_provider/{name} immediately after the response"
    );
    assert_eq!(
        actor.as_deref(),
        Some("audit-test-admin"),
        "actor must be recorded for a provider create"
    );

    // cleanup so repeat runs against a long-lived scratch DB stay clean
    let store = ares_store::runtime_providers::RuntimeProviderStore::new(&pool);
    let _ = store.delete_scoped(None, &name).await;
}

#[tokio::test]
async fn audit_row_lands_before_response_admin_runtime_provider_delete() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let name = unique("audit-test-provider-del");
    let store = ares_store::runtime_providers::RuntimeProviderStore::new(&pool);
    store
        .upsert(&CreateRuntimeProviderRequest {
            tenant_id: None,
            name: name.clone(),
            display_name: "Audit Test Provider Del".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://example.test/v1".to_string(),
            auth_type: "api_key".to_string(),
            default_model: None,
            headers: None,
            request_transform: None,
            response_transform: None,
            enabled: Some(true),
        })
        .await
        .expect("seed provider directly");

    let status = delete_runtime_provider(
        State(ctx.clone()),
        AdminActor {
            subject: Some("audit-test-admin".to_string()),
            email: None,
            auth: Some("jwt"),
            client_ip: Some("203.0.113.99".to_string()),
        },
        Path(name.clone()),
        Query(RuntimeProviderScopeQuery { tenant_id: None }),
    )
    .await
    .expect("delete_runtime_provider response");
    assert_eq!(status, axum::http::StatusCode::NO_CONTENT);

    let (count, actor) =
        admin_audit_row_count(&pool, "delete_runtime_provider", "runtime_provider", &name).await;
    assert_eq!(
        count, 1,
        "expected exactly one admin_audit_log row for delete_runtime_provider/{name} immediately after the response"
    );
    assert_eq!(
        actor.as_deref(),
        Some("audit-test-admin"),
        "actor must be recorded for a provider delete"
    );
}

// ---------------------------------------------------------------------------
// A failed audit write must be logged at error, never silently dropped.
// ---------------------------------------------------------------------------

/// Minimal hand-rolled `tracing::Subscriber` (no new dependency: `tracing`
/// itself is already a direct `ares-http` dependency; `tracing-subscriber`
/// is not — adding it would be a new dependency the brief forbids). Captures
/// every event's level, target and fields so the test can assert one fired
/// at `ERROR` naming the audit action, without needing a fmt layer.
#[derive(Clone, Default)]
struct CaptureLog(Arc<Mutex<Vec<(tracing::Level, String, Vec<(String, String)>)>>>);

struct FieldVisitor(Vec<(String, String)>);
impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0
            .push((field.name().to_string(), format!("{value:?}")));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push((field.name().to_string(), value.to_string()));
    }
}

impl tracing::Subscriber for CaptureLog {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = FieldVisitor(Vec::new());
        event.record(&mut visitor);
        self.0.lock().unwrap().push((
            *event.metadata().level(),
            event.metadata().target().to_string(),
            visitor.0,
        ));
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

#[tokio::test]
async fn audit_failure_is_logged_not_dropped() {
    let Some(url) = common::live_db_url(&common::current_test_name()).await else {
        return;
    };
    // Ensure migrations have run once for this binary (ares_test_support's
    // shared INIT) before opening our own dedicated connection below — this
    // test may run before any other test in the binary has triggered it.
    let _ = ares_test_support::pool().await;

    // A dedicated single-connection pool: every query on it runs on the same
    // Postgres session, so a session-scoped TEMP TABLE shadows the real
    // `admin_audit_log` for every subsequent query this pool makes — and
    // only this pool; ares_test_support's shared pool is untouched.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect one-connection pool");

    let tenant_db = Arc::new(TenantDb::new(Arc::new(ares_store::PostgresClient {
        pool: pool.clone(),
    })));
    let tenant = tenant_db
        .create_tenant(unique("t1116-fail"), TenantTier::Free)
        .await
        .expect("create tenant");
    let (api_key, _raw) = tenant_db
        .create_api_key(&tenant.id, "to-revoke-shadowed".to_string(), None, None)
        .await
        .expect("create api key directly");

    // Shadow admin_audit_log for the rest of this connection's session.
    // Column shape (`id int` only) guarantees the 8-column INSERT errors.
    sqlx::query("CREATE TEMP TABLE admin_audit_log (id int)")
        .execute(&pool)
        .await
        .expect("shadow admin_audit_log with a temp table");

    let capture = CaptureLog::default();
    let ctx = Context::new_root();
    ctx.provide_arc(tenant_db.clone());
    let tc = TenantContext::new(tenant.id.clone(), TenantTier::Free);

    // `tracing::subscriber::with_default` only wraps a synchronous closure,
    // and this call is async, so set the default for the task instead and
    // reset it after — a `DefaultGuard` held across the `.await` is exactly
    // what `tracing`'s docs recommend for this shape.
    let guard = tracing::subscriber::set_default(capture.clone());
    let response = revoke_api_key(
        State(ctx.clone()),
        Some(Extension(tc)),
        None,
        Path(api_key.id.clone()),
        HeaderMap::new(),
    )
    .await;
    drop(guard);

    // The mutation already happened (api_keys.is_active flipped) before the
    // audit write was attempted, so the handler must still report success.
    assert_eq!(
        response.expect("revoke_api_key must still succeed"),
        axum::http::StatusCode::NO_CONTENT,
        "the handler's response must be unaffected by the audit write failing"
    );

    // And the real (unshadowed, shared-pool) table must show no row: the
    // insert never landed anywhere real.
    drop(pool);
    let real_pool = ares_test_support::pool().await;
    let (count, _) =
        admin_audit_row_count(&real_pool, "revoke_api_key", "api_key", &api_key.id).await;
    assert_eq!(
        count, 0,
        "the shadowed insert must not have landed in the real admin_audit_log"
    );

    let events = capture.0.lock().unwrap();
    let found = events.iter().any(|(level, _target, fields)| {
        *level == tracing::Level::ERROR
            && fields
                .iter()
                .any(|(k, v)| k == "action" && v.contains("revoke_api_key"))
    });
    assert!(
        found,
        "expected an ERROR-level tracing event naming the failed action \
         (revoke_api_key); captured events: {events:?}"
    );
}

// ---------------------------------------------------------------------------
// 1.16-FIX-1: the admin billing and cordis writes, the bulk key revoke, and
// the rotate ordering. Each asserts right after the response, no sleep.
// ---------------------------------------------------------------------------

/// One `admin_audit_log` row as the tests see it.
#[derive(Debug)]
struct AuditRow {
    action: String,
    resource_type: String,
    actor: Option<String>,
    details: Option<String>,
}

/// Every audit row whose `resource_id` is `resource_id`, oldest first. The
/// tests use ids no other test touches, so "exactly one new row" is the
/// length of this list.
async fn rows_for_resource(pool: &PgPool, resource_id: &str) -> Vec<AuditRow> {
    sqlx::query(
        "SELECT action, resource_type, actor, details FROM admin_audit_log \
         WHERE resource_id = $1 ORDER BY created_at, action",
    )
    .bind(resource_id)
    .fetch_all(pool)
    .await
    .expect("audit rows query")
    .into_iter()
    .map(|row| AuditRow {
        action: row.get("action"),
        resource_type: row.get("resource_type"),
        actor: row.get("actor"),
        details: row.get("details"),
    })
    .collect()
}

fn admin_actor() -> AdminActor {
    AdminActor {
        subject: Some("audit-test-admin".to_string()),
        email: None,
        auth: Some("jwt"),
        client_ip: Some("203.0.113.99".to_string()),
    }
}

#[tokio::test]
async fn audit_row_lands_before_response_billing() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant_db = ctx.get::<TenantDb>().expect("TenantDb");
    let tenant = tenant_db
        .create_tenant(unique("t1116-billing"), TenantTier::Free)
        .await
        .expect("create tenant");

    let resp = set_token_budget(
        State(ctx.clone()),
        admin_actor(),
        Path(tenant.id.clone()),
        Json(SetTokenBudgetRequest {
            token_limit: 1_000,
            period: "monthly".to_string(),
        }),
    )
    .await
    .expect("set_token_budget response");
    assert_eq!(resp.0.token_limit, 1_000);

    // Right after the response, no sleep: exactly one row for this tenant.
    let rows = rows_for_resource(&pool, &tenant.id).await;
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one admin_audit_log row for the billing write immediately after the \
         response, got {rows:?}"
    );
    assert_eq!(rows[0].action, "set_token_budget");
    assert_eq!(rows[0].resource_type, "token_budget");
    assert_eq!(rows[0].actor.as_deref(), Some("audit-test-admin"));
    let details = rows[0].details.as_deref().expect("details");
    assert!(
        details.contains("1000") && details.contains("monthly"),
        "details must record the new limit and period, got {details}"
    );
}

#[tokio::test]
async fn audit_row_lands_before_response_cordis() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    // `events_service` is the one service the endpoint can provide; a fresh
    // root context does not have it yet, so the call provides it.
    assert!(ctx.get::<cordis::EventsService>().is_none());
    let before = rows_for_resource(&pool, "events_service").await.len();

    let (status, Json(body)) = provide_cordis_service(
        State(ctx.clone()),
        admin_actor(),
        Path("events_service".to_string()),
    )
    .await
    .expect("provide_cordis_service response");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["provided"], serde_json::json!(true));

    let rows = rows_for_resource(&pool, "events_service").await;
    assert_eq!(
        rows.len(),
        before + 1,
        "expected exactly one new admin_audit_log row for the cordis write immediately after the \
         response, got {rows:?}"
    );
    let row = rows.last().expect("one row");
    assert_eq!(row.action, "provide_cordis_service");
    assert_eq!(row.resource_type, "cordis_service");
    assert_eq!(row.actor.as_deref(), Some("audit-test-admin"));
}

#[tokio::test]
async fn audit_row_lands_before_response_bulk_key_revoke() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant_db = ctx.get::<TenantDb>().expect("TenantDb");
    let tenant = tenant_db
        .create_tenant(unique("t1116-bulk"), TenantTier::Free)
        .await
        .expect("create tenant");
    for n in 0..3 {
        tenant_db
            .create_api_key(&tenant.id, format!("bulk-{n}"), None, None)
            .await
            .expect("create api key directly");
    }
    let tc = TenantContext::new(tenant.id.clone(), TenantTier::Free);

    let resp = delete_tenant_data(
        State(ctx.clone()),
        Some(Extension(tc)),
        None,
        HeaderMap::new(),
    )
    .await
    .expect("delete_tenant_data response");
    assert_eq!(resp.0["api_keys_revoked"], serde_json::json!(3));

    // One row naming the tenant, and the number of keys revoked in details.
    let rows = rows_for_resource(&pool, &tenant.id).await;
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one admin_audit_log row for the bulk key revoke immediately after the \
         response, got {rows:?}"
    );
    assert_eq!(rows[0].action, "delete_tenant_data");
    assert_eq!(rows[0].resource_type, "tenant");
    assert_eq!(rows[0].actor.as_deref(), Some(tenant.id.as_str()));
    let details: serde_json::Value =
        serde_json::from_str(rows[0].details.as_deref().expect("details")).expect("json details");
    assert_eq!(details["api_keys_revoked"], serde_json::json!(3));
}

#[tokio::test]
async fn audit_row_lands_before_response_v1_key_rotate() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant_db = ctx.get::<TenantDb>().expect("TenantDb");
    let tenant = tenant_db
        .create_tenant(unique("t1116-rotate"), TenantTier::Free)
        .await
        .expect("create tenant");
    let (old_key, _raw) = tenant_db
        .create_api_key(&tenant.id, "to-rotate".to_string(), None, None)
        .await
        .expect("create api key directly");
    let tc = TenantContext::new(tenant.id.clone(), TenantTier::Free);

    let resp = rotate_api_key(
        State(ctx.clone()),
        Some(Extension(tc)),
        None,
        Path(old_key.id.clone()),
        HeaderMap::new(),
        Json(RotateApiKeyRequest {
            scopes: None,
            expires_in_days: None,
        }),
    )
    .await
    .expect("rotate_api_key response");
    let new_id = resp.0.key.id.clone();

    // The mint and the revoke each leave one row, on their own key.
    let minted = rows_for_resource(&pool, &new_id).await;
    assert_eq!(minted.len(), 1, "mint rows: {minted:?}");
    assert_eq!(minted[0].action, "rotate_api_key");
    assert_eq!(minted[0].resource_type, "api_key");
    assert_eq!(minted[0].actor.as_deref(), Some(tenant.id.as_str()));
    assert!(minted[0]
        .details
        .as_deref()
        .expect("details")
        .contains(&old_key.id));

    let revoked = rows_for_resource(&pool, &old_key.id).await;
    assert_eq!(revoked.len(), 1, "revoke rows: {revoked:?}");
    assert_eq!(revoked[0].action, "revoke_api_key");
    assert_eq!(revoked[0].resource_type, "api_key");
    assert_eq!(revoked[0].actor.as_deref(), Some(tenant.id.as_str()));
    assert!(revoked[0]
        .details
        .as_deref()
        .expect("details")
        .contains(&new_id));
}

/// `rotate_api_key` mints first, then revokes the old key with `?`. When the
/// revoke fails the tenant has a live minted key the caller never received a
/// response for: it must still have its audit row.
///
/// The revoke is forced to fail here, in the scratch database, with a trigger
/// that raises on the UPDATE of this one key's row (`revoke_api_key` is
/// `UPDATE api_keys SET is_active = 0 WHERE id = $1 AND tenant_id = $2`). It
/// touches no product code, and its `WHEN` clause names this test's own key id
/// so no other test in the binary is affected; it is dropped before any
/// assertion.
#[tokio::test]
async fn rotate_audits_the_mint_when_the_revoke_fails() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant_db = ctx.get::<TenantDb>().expect("TenantDb");
    let tenant = tenant_db
        .create_tenant(unique("t1116-rotate-fail"), TenantTier::Free)
        .await
        .expect("create tenant");
    let (old_key, _raw) = tenant_db
        .create_api_key(&tenant.id, "to-rotate-fail".to_string(), None, None)
        .await
        .expect("create api key directly");
    let tc = TenantContext::new(tenant.id.clone(), TenantTier::Free);

    // Force the revoke's UPDATE (only of this key) to fail.
    let trigger = format!("t1116_fail_revoke_{}", old_key.id.replace('-', ""));
    sqlx::query(
        "CREATE OR REPLACE FUNCTION t1116_fail_revoke() RETURNS trigger LANGUAGE plpgsql AS \
         $$ BEGIN RAISE EXCEPTION 't1116 forced revoke failure'; END $$",
    )
    .execute(&pool)
    .await
    .expect("create trigger function in the scratch database");
    sqlx::query(&format!(
        "CREATE TRIGGER {trigger} BEFORE UPDATE ON api_keys FOR EACH ROW \
         WHEN (OLD.id = '{}') EXECUTE FUNCTION t1116_fail_revoke()",
        old_key.id
    ))
    .execute(&pool)
    .await
    .expect("create trigger in the scratch database");

    let result = rotate_api_key(
        State(ctx.clone()),
        Some(Extension(tc)),
        None,
        Path(old_key.id.clone()),
        HeaderMap::new(),
        Json(RotateApiKeyRequest {
            scopes: None,
            expires_in_days: None,
        }),
    )
    .await;

    sqlx::query(&format!("DROP TRIGGER IF EXISTS {trigger} ON api_keys"))
        .execute(&pool)
        .await
        .expect("drop trigger");

    // The handler's status for a failed revoke is today's: the database
    // error from `revoke_api_key`, mapped to 500. The audit ordering must not
    // change it.
    let err = result.expect_err("a failed revoke fails the rotation");
    assert!(
        matches!(&err.0, AppError::Database(m) if m.contains("Failed to revoke API key")),
        "unexpected error: {err:?}"
    );
    assert_eq!(
        err.into_response().status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );

    // The mint happened: exactly one other key exists for the tenant, active.
    let minted_ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM api_keys WHERE tenant_id = $1 AND id <> $2")
            .bind(&tenant.id)
            .bind(&old_key.id)
            .fetch_all(&pool)
            .await
            .expect("minted key lookup");
    assert_eq!(minted_ids.len(), 1, "the mint should have succeeded");

    // ... and it has its audit row, though the response was an error.
    let minted = rows_for_resource(&pool, &minted_ids[0]).await;
    assert_eq!(
        minted.len(),
        1,
        "the minted key must have exactly one audit row even though the revoke failed, got \
         {minted:?}"
    );
    assert_eq!(minted[0].action, "rotate_api_key");
    assert_eq!(minted[0].resource_type, "api_key");
    assert_eq!(minted[0].actor.as_deref(), Some(tenant.id.as_str()));
    assert!(minted[0]
        .details
        .as_deref()
        .expect("details")
        .contains(&old_key.id));

    // The revoke did not happen, so the old key has no revoke row.
    assert!(
        rows_for_resource(&pool, &old_key.id).await.is_empty(),
        "no revoke row for a revoke that failed"
    );
}
