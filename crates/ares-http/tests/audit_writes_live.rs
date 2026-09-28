//! Live-DB tests for item 1.16 (VERIFY-2026-09-22.md §4 row 20): every named admin/key-lifecycle
//! write must leave exactly one `admin_audit_log` row, with its actor,
//! landed BEFORE the handler's response is returned (no sleep, no polling) —
//! and a failed audit insert must be visible at `error` level, never dropped.
//!
//! Requires a live Postgres reachable via `TEST_DATABASE_URL` (never
//! `ares_test`; see the brief). Handlers are called directly as plain async
//! functions with hand-built extractors (`State`, `Extension`, `Path`,
//! `Json`) — the same in-process pattern `ares-http`'s own
//! `middleware/api_key_auth.rs` test module uses for its live-DB cases —
//! which lets each test assert on the database in the same task, right after
//! the `.await` that produced the response, with nothing in between.
//!
//! `#[cfg(feature = "postgres")]`, not `#[ignore]`: these are meant to run in
//! the normal `cargo test --locked` sweep (they are the brief's failing/
//! passing evidence), so they must skip cleanly instead of panicking when
//! the live database is unreachable — matching `live_chat_stream.rs`.

#![cfg(feature = "postgres")]

use std::sync::{Arc, Mutex};

use ares_http::api::handlers::admin::providers::{
    delete_runtime_provider, upsert_runtime_provider,
};
use ares_http::api::handlers::admin::shared::{
    CreateRuntimeProviderRequest, RuntimeProviderScopeQuery,
};
use ares_http::api::handlers::admin::{update_tenant_agent_handler, AdminActor};
use ares_http::api::handlers::v1::{create_api_key, revoke_api_key, CreateApiKeyRequest};
use ares_store::tenant_agents::{create_tenant_agent, CreateTenantAgentRequest};
use ares_store::TenantDb;
use ares_types::models::{TenantContext, TenantTier};
use axum::extract::{Extension, Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use cordis::Context;
use sqlx::PgPool;
use sqlx::Row;

async fn db_reachable(url: &str) -> bool {
    match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(url),
    )
    .await
    {
        Ok(Ok(pool)) => {
            pool.close().await;
            true
        }
        _ => false,
    }
}

/// A fresh root `Context` carrying a real, connected `TenantDb`, plus the
/// pool underneath it for the test's own assertions.
async fn live_ctx() -> Option<(Arc<Context>, PgPool)> {
    let url = ares_test_support::test_db_url();
    if !db_reachable(&url).await {
        eprintln!("SKIPPED: test database unreachable ({url})");
        return None;
    }
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
    let url = ares_test_support::test_db_url();
    if !db_reachable(&url).await {
        eprintln!("SKIPPED: test database unreachable ({url})");
        return;
    }
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
