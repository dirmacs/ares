//! Item 2.16a: every webhook is bound to its tenant's own secret, and the
//! tenant comes from the secret, never from the request body.
//!
//! Ruling: `2026-10-02-DELEGATED-sr-2.16a-webhooks-and-2.5-split` §2, after
//! `2026-09-29-DELEGATED-sr-2.16e-webhook-unset` (an unset or empty secret
//! refuses everything).
//!
//! The three routes under test, as `ares_http::build_router` mounts them:
//!
//! - `POST /api/events/document-upload`
//! - `POST /api/events/field-change`
//! - `POST /api/webhooks/{trigger_id}`
//!
//! Each tenant has one event secret, stored only as its SHA-256 in
//! `tenant_event_secrets` (migration 041). A request presents it in
//! `X-Webhook-Secret`:
//!
//! - no secret, an empty or blank one, an unknown one: 401 (the 2.16e body,
//!   byte for byte);
//! - the secret of tenant A with a body `tenant_id` that is not A: 403;
//! - the secret of tenant A on a trigger of tenant B (`/webhooks/{id}`): 403;
//! - the secret of tenant A, body `tenant_id` absent or equal to A: it runs
//!   A's triggers and only A's;
//! - the process-wide `WEBHOOK_SECRET` is not a credential any more: it alone
//!   is refused on all three routes, and the tests hold it both unset and set.
//!
//! "A trigger ran" is read from `agent_runs`: the trigger path writes one row
//! (tenant, `trigger_id`) before it calls the engine, and the engine here has
//! no model, so the run fails, but the row is there. That makes "tenant B's
//! trigger did not run" a fact about the database, not about a status code.
//!
//! Every test drives the real router in process, once without the Cordis
//! `TriggerService` (the handlers' direct path) and, for the refusals, once
//! with it (the path production takes). The tests need a live scratch Postgres
//! and touch only the one `TEST_DATABASE_URL` names (never `ares_test`):
//!
//! - `TEST_DATABASE_URL` unset or empty: every test panics first. They never
//!   skip and never fall back to `DATABASE_URL` or the unix-socket default.
//! - Set but unreachable: every test panics (the gate in `tests/common`).
//! - Neither panic prints the URL.
//!
//! Every secret here is a dummy spelled in this file. `WEBHOOK_SECRET` is
//! process-global: every test takes the one [`ENV_LOCK`] for its whole body
//! and sets or removes the variable itself.

#![cfg(feature = "postgres")]
// Deliberate: each test holds `ENV_LOCK` across its awaited requests, because
// the code under test may read the variable mid-await (as in `empty_secrets`).
#![allow(clippy::await_holding_lock)]

mod common;

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use ares_agent::context_provider::NoOpContextProvider;
use ares_agent::execution::Execute;
use ares_agent::trigger::TriggerService;
use ares_agent::ContextProviderHandle;
use ares_http::auth::jwt::AuthService;
use ares_store::schedules::{CreateTriggerRequest, EventTriggerStore};
use ares_store::tenant_agents::{create_tenant_agent, CreateTenantAgentRequest};
use ares_store::{PostgresClient, TenantDb};
use ares_types::models::TenantTier;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower::ServiceExt;

const WEBHOOK_ENV: &str = "WEBHOOK_SECRET";

/// Dummy secrets for this file only. They protect nothing.
const SECRET_A: &str = "tenant-a-event-secret-dummy-2-16a";
const SECRET_B: &str = "tenant-b-event-secret-dummy-2-16a";
/// What the old process-wide variable would hold.
const GLOBAL_SECRET: &str = "process-wide-webhook-secret-dummy-2-16a";
/// A secret no tenant has.
const WRONG_SECRET: &str = "not-any-tenants-secret-dummy-2-16a";
const TEST_JWT_SECRET: &str = "webhook-tenant-secret-test-jwt-secret-at-least-32-chars";

/// Every tenant's agent, so a fired trigger gets as far as its `agent_runs` row.
const AGENT: &str = "webhook-agent-2-16a";
const COLUMN: &str = "status";

/// The 401 body, byte for byte as 2.16e pinned it (`tests/empty_secrets.rs`).
fn webhook_refusal() -> Value {
    json!({
        "error": "Authentication error: Invalid webhook secret",
        "code": "AUTHENTICATION_FAILED",
    })
}

// ---------------------------------------------------------------------------
// Process-global state: the environment lock and the log capture
// ---------------------------------------------------------------------------

/// Serialises every mutation of `WEBHOOK_SECRET` in this binary. Poisoning is
/// recovered: each test sets what it needs first.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_env() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Set (`Some`) or remove (`None`) the variable. The caller holds [`ENV_LOCK`].
fn set_env(value: Option<&str>) {
    match value {
        Some(v) => std::env::set_var(WEBHOOK_ENV, v),
        None => std::env::remove_var(WEBHOOK_ENV),
    }
}

fn env_label(value: Option<&str>) -> &'static str {
    if value.is_some() {
        "WEBHOOK_SECRET set"
    } else {
        "WEBHOOK_SECRET unset"
    }
}

/// The binary's one log capture (the crate's hand-rolled subscriber, as in
/// `v1_failed_run_metering_tests.rs`: `tracing` is a direct dependency and
/// `tracing-subscriber` is not). Installed as the global default by the first
/// world, before any request, so no callsite is cached as never-enabled.
fn global_capture() -> &'static CaptureLog {
    static CAPTURE: OnceLock<CaptureLog> = OnceLock::new();
    CAPTURE.get_or_init(|| {
        let capture = CaptureLog::default();
        tracing::subscriber::set_global_default(capture.clone())
            .expect("nothing else sets a global subscriber in this test binary");
        capture
    })
}

#[derive(Clone, Default)]
struct CaptureLog(Arc<Mutex<Vec<CapturedEvent>>>);

/// One captured event: its level, its target and its `(field, value)` pairs
/// (the message is the field `message`).
type CapturedEvent = (tracing::Level, String, Vec<(String, String)>);

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

// ---------------------------------------------------------------------------
// The world under test: two tenants, each with a secret and four triggers
// ---------------------------------------------------------------------------

/// The one database this binary may touch: the one `TEST_DATABASE_URL` names.
///
/// Every test calls this first. Unset, empty or not valid Unicode panics: these
/// security tests never skip and never fall back to another database. The
/// message names the variable, never a URL.
fn named_test_db() -> String {
    match std::env::var(common::DB_ENV) {
        Ok(url) if !url.trim().is_empty() => url,
        _ => panic!(
            "{test}: {var} is unset or empty. The webhook tenant-secret tests never skip and \
             never fall back to another database: set {var} to a scratch database (never \
             ares_test).",
            test = common::current_test_name(),
            var = common::DB_ENV,
        ),
    }
}

/// Migrations run once per test binary, on the named database.
static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// A pool on exactly the named database, after the gate and the migrations.
async fn migrated_pool(db_url: String) -> PgPool {
    let test = common::current_test_name();
    // Configured and unreachable panics here, naming the variable only.
    let common::Gate::Run(db_url) = common::gate(&test, true, db_url).await else {
        unreachable!("a configured gate never skips");
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .unwrap_or_else(|_| panic!("{test}: no pool on the database {} names", common::DB_ENV));
    MIGRATED
        .get_or_init(|| async {
            ares_store::MIGRATOR
                .run(&pool)
                .await
                .expect("migrate the named test database");
        })
        .await;
    pool
}

fn sha256_hex(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// The harness's way of provisioning a tenant: the SHA-256 goes in, never the
/// secret. Migration 041 must exist.
async fn store_secret(pool: &PgPool, tenant_id: &str, secret: &str) {
    sqlx::query("INSERT INTO tenant_event_secrets (tenant_id, secret_sha256) VALUES ($1, $2)")
        .bind(tenant_id)
        .bind(sha256_hex(secret))
        .execute(pool)
        .await
        .expect("store the tenant's event secret hash: migration 041 (tenant_event_secrets)");
}

/// A hash is unique across tenants, so a world first clears whatever an earlier
/// world, or an earlier run on this scratch database, left under the dummy
/// secrets it is about to provision. Safe: every test holds [`ENV_LOCK`] for
/// its whole body, so exactly one world is live at a time.
async fn clear_dummy_secrets(pool: &PgPool, secrets: &[&str]) {
    for secret in secrets {
        sqlx::query("DELETE FROM tenant_event_secrets WHERE secret_sha256 = $1")
            .bind(sha256_hex(secret))
            .execute(pool)
            .await
            .expect("clear a dummy secret left by an earlier world (migration 041)");
    }
}

async fn make_trigger(
    pool: &PgPool,
    tenant_id: &str,
    name: &str,
    event_type: &str,
    event_config: Value,
    enabled: bool,
) -> String {
    EventTriggerStore::new(pool)
        .create_trigger(&CreateTriggerRequest {
            tenant_id: tenant_id.to_string(),
            name: name.to_string(),
            event_type: event_type.to_string(),
            event_config,
            target_agent: AGENT.to_string(),
            enabled,
        })
        .await
        .expect("seed trigger")
        .id
}

/// One tenant of the world.
struct Tenant {
    id: String,
    secret: &'static str,
    webhook: String,
    webhook_disabled: String,
    document: String,
}

async fn seed_tenant(
    tenant_db: &TenantDb,
    pool: &PgPool,
    label: &str,
    secret: &'static str,
    bucket: &str,
    table: &str,
) -> Tenant {
    let tenant = tenant_db
        .create_tenant(
            format!("2-16a-{label}-{}", uuid::Uuid::new_v4()),
            TenantTier::Free,
        )
        .await
        .expect("seed tenant");
    // Without `skill_id`, so a fired trigger takes the regular path, which
    // writes its `agent_runs` row before it calls the engine.
    create_tenant_agent(
        pool,
        &tenant.id,
        CreateTenantAgentRequest {
            agent_name: AGENT.to_string(),
            display_name: "2.16a probe".to_string(),
            description: None,
            config: json!({
                "model": "2-16a-model",
                "system_prompt": "2.16a probe",
                "tools": [],
                "max_tool_iterations": 1,
                "parallel_tools": false
            }),
        },
    )
    .await
    .expect("seed tenant agent");
    store_secret(pool, &tenant.id, secret).await;
    let webhook = make_trigger(pool, &tenant.id, "hook", "webhook", json!({}), true).await;
    let webhook_disabled =
        make_trigger(pool, &tenant.id, "hook-off", "webhook", json!({}), false).await;
    let document = make_trigger(
        pool,
        &tenant.id,
        "docs",
        "document_upload",
        json!({ "bucket": bucket }),
        true,
    )
    .await;
    make_trigger(
        pool,
        &tenant.id,
        "fields",
        "field_change",
        json!({ "table": table, "column": COLUMN }),
        true,
    )
    .await;
    Tenant {
        id: tenant.id,
        secret,
        webhook,
        webhook_disabled,
        document,
    }
}

struct World {
    router: axum::Router,
    pool: PgPool,
    a: Tenant,
    b: Tenant,
    /// The bucket and table both tenants' triggers watch, so one event body
    /// matches both tenants' triggers and only the tenant decides whose runs.
    bucket: String,
    table: String,
}

impl World {
    /// `with_trigger_service`: provide the Cordis `TriggerService`, the path
    /// production takes; without it the handlers use the store directly.
    async fn boot(db_url: String, with_trigger_service: bool) -> World {
        global_capture();
        let pool = migrated_pool(db_url).await;
        clear_dummy_secrets(&pool, &[SECRET_A, SECRET_B]).await;
        let pg = Arc::new(PostgresClient { pool: pool.clone() });
        let tenant_db = Arc::new(TenantDb::new(pg.clone()));

        let bucket = format!("bucket-2-16a-{}", uuid::Uuid::new_v4());
        let table = format!("table_2_16a_{}", uuid::Uuid::new_v4().simple());
        let a = seed_tenant(&tenant_db, &pool, "a", SECRET_A, &bucket, &table).await;
        let b = seed_tenant(&tenant_db, &pool, "b", SECRET_B, &bucket, &table).await;

        let ctx = cordis::Context::new_root();
        ctx.provide_arc(tenant_db);
        ctx.provide_arc(Arc::new(AuthService::new(
            TEST_JWT_SECRET.to_string(),
            900,
            604_800,
        )));
        // No model anywhere: a fired trigger writes its `agent_runs` row and
        // then fails fast in the engine.
        let execute = Arc::new(Execute::new().with_strict_fallbacks(true));
        ctx.provide(ContextProviderHandle::new(Arc::new(NoOpContextProvider)));
        ctx.provide_arc(execute.clone());
        if with_trigger_service {
            ctx.provide(TriggerService::new(pg, execute));
        }
        World {
            router: ares_http::build_router(ctx),
            pool,
            a,
            b,
            bucket,
            table,
        }
    }

    /// How many runs a tenant's triggers have written.
    async fn runs_of(&self, tenant_id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_runs WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_one(&self.pool)
            .await
            .expect("count the tenant's runs")
    }

    async fn counts(&self) -> (i64, i64) {
        (
            self.runs_of(&self.a.id).await,
            self.runs_of(&self.b.id).await,
        )
    }

    /// True when a response body carries anything it must not: any secret,
    /// any secret's hash, or either tenant's id.
    fn leaks(&self, body: &str) -> bool {
        [SECRET_A, SECRET_B, GLOBAL_SECRET, WRONG_SECRET]
            .iter()
            .flat_map(|s| [s.to_string(), sha256_hex(s)])
            .chain([self.a.id.clone(), self.b.id.clone()])
            .any(|needle| body.contains(&needle))
    }
}

// ---------------------------------------------------------------------------
// Requests and verdicts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum Route {
    Document,
    Field,
    Webhook,
}

const EVENT_ROUTES: [Route; 2] = [Route::Document, Route::Field];
const ALL_ROUTES: [Route; 3] = [Route::Document, Route::Field, Route::Webhook];

struct Outcome {
    status: StatusCode,
    body: String,
}

fn post_json(uri: &str, secret: Option<&str>, body: Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(value) = secret {
        builder = builder.header("x-webhook-secret", value);
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

/// `body_tenant`: `None` leaves `tenant_id` out of the body; `Some(v)` puts `v`
/// there. `trigger_id` is used by the webhook route only.
async fn call(
    w: &World,
    route: Route,
    secret: Option<&str>,
    body_tenant: Option<Value>,
    trigger_id: &str,
) -> Outcome {
    let request = match route {
        Route::Document => {
            let mut body = json!({
                "bucket": w.bucket,
                "key": "uploads/2-16a/probe.pdf",
                "size": 1,
                "content_type": "application/pdf",
                "signed_url": "",
            });
            if let Some(tenant) = body_tenant {
                body["tenant_id"] = tenant;
            }
            post_json("/api/events/document-upload", secret, body)
        }
        Route::Field => {
            let mut body = json!({
                "table": w.table,
                "column": COLUMN,
                "record_id": "r-2-16a",
                "old_value": "old",
                "new_value": "new",
            });
            if let Some(tenant) = body_tenant {
                body["tenant_id"] = tenant;
            }
            post_json("/api/events/field-change", secret, body)
        }
        Route::Webhook => post_json(
            &format!("/api/webhooks/{trigger_id}"),
            secret,
            json!({ "event": "2-16a-probe" }),
        ),
    };
    let response = w.router.clone().oneshot(request).await.expect("router");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    Outcome {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn json_body(o: &Outcome) -> Option<Value> {
    serde_json::from_str(&o.body).ok()
}

/// 401 with the 2.16e body, byte for byte, and nothing it must not carry.
fn refused_401(w: &World, o: &Outcome) -> bool {
    o.status == StatusCode::UNAUTHORIZED
        && json_body(o) == Some(webhook_refusal())
        && !w.leaks(&o.body)
}

/// 403 with the authorization code, and nothing it must not carry.
fn refused_403(w: &World, o: &Outcome) -> bool {
    o.status == StatusCode::FORBIDDEN
        && json_body(o).is_some_and(|v| v["code"] == "AUTHORIZATION_FAILED")
        && !w.leaks(&o.body)
}

/// An admitted request: 200, and on `/webhooks/{id}` the trigger answered.
fn admitted(route: Route, o: &Outcome) -> bool {
    match route {
        Route::Webhook => {
            o.status == StatusCode::OK
                && json_body(o).is_some_and(|v| v["status"] == "triggered" && v["agent"] == AGENT)
        }
        _ => o.status == StatusCode::OK,
    }
}

/// The verdict cells of one test: every cell is printed, every wrong one is
/// listed, and the count is pinned so a skipped loop cannot pass.
struct Cells {
    n: usize,
    wrong: Vec<String>,
}

impl Cells {
    fn new() -> Self {
        Cells {
            n: 0,
            wrong: Vec::new(),
        }
    }

    fn fact(&mut self, label: String, ok: bool, detail: String) {
        self.n += 1;
        eprintln!(
            "CELL {label} | {detail} | {}",
            if ok { "ok" } else { "WRONG" }
        );
        if !ok {
            self.wrong.push(format!("{label}: {detail}"));
        }
    }

    fn status(&mut self, label: String, ok: bool, o: &Outcome) {
        let detail = format!("status {} body {:?}", o.status.as_u16(), o.body);
        self.fact(label, ok, detail);
    }

    fn finish(self, test: &str, expected: usize) {
        eprintln!(
            "CELLS {test} | {} cells | {} wrong",
            self.n,
            self.wrong.len()
        );
        assert_eq!(self.n, expected, "{test}: the cell count is pinned");
        assert!(
            self.wrong.is_empty(),
            "{} of {} cells wrong:\n{}",
            self.wrong.len(),
            self.n,
            self.wrong.join("\n")
        );
    }
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// Tenant A's secret cannot fire tenant B's trigger: not by naming B in the
/// body of an event, not by posting to B's trigger id. B's trigger does not run
/// (no `agent_runs` row), whatever the old process-wide variable holds.
#[tokio::test(flavor = "multi_thread")]
async fn tenant_a_secret_cannot_fire_tenant_b_trigger() {
    let db = named_test_db();
    let _env = lock_env();
    let mut cells = Cells::new();
    let mut expected = 0;

    for with_service in [false, true] {
        let w = World::boot(db.clone(), with_service).await;
        // The worst legacy case: the one process-wide secret equals A's.
        for env in [None, Some(SECRET_A)] {
            set_env(env);
            let ctx = format!("service={with_service} {}", env_label(env));
            for route in EVENT_ROUTES {
                let o = call(&w, route, Some(SECRET_A), Some(json!(w.b.id)), "").await;
                cells.status(
                    format!("{ctx} | {route:?} | A's secret, body names B"),
                    refused_403(&w, &o),
                    &o,
                );
                let o = call(&w, route, Some(SECRET_B), Some(json!(w.a.id)), "").await;
                cells.status(
                    format!("{ctx} | {route:?} | B's secret, body names A"),
                    refused_403(&w, &o),
                    &o,
                );
                expected += 2;
            }
            let o = call(&w, Route::Webhook, Some(SECRET_A), None, &w.b.webhook).await;
            cells.status(
                format!("{ctx} | Webhook | A's secret on B's trigger"),
                refused_403(&w, &o),
                &o,
            );
            let o = call(&w, Route::Webhook, Some(SECRET_B), None, &w.a.webhook).await;
            cells.status(
                format!("{ctx} | Webhook | B's secret on A's trigger"),
                refused_403(&w, &o),
                &o,
            );
            expected += 2;
        }
        set_env(None);
        let counts = w.counts().await;
        cells.fact(
            format!("service={with_service} | nothing ran"),
            counts == (0, 0),
            format!("runs (A, B) = {counts:?}"),
        );
        expected += 1;
    }
    cells.finish("tenant_a_secret_cannot_fire_tenant_b_trigger", expected);
}

/// A body `tenant_id` that is not the secret's tenant is refused with 403, in
/// every form it can differ (another tenant, empty, padded, another case, a
/// tenant that does not exist), and nothing runs.
#[tokio::test(flavor = "multi_thread")]
async fn body_tenant_id_mismatching_the_secrets_tenant_is_refused() {
    let db = named_test_db();
    let _env = lock_env();
    let mut cells = Cells::new();
    let mut expected = 0;

    for with_service in [false, true] {
        let w = World::boot(db.clone(), with_service).await;
        let mismatches: [(&str, Value); 5] = [
            ("another tenant", json!(w.b.id)),
            ("empty", json!("")),
            ("padded", json!(format!(" {} ", w.a.id))),
            ("another case", json!(w.a.id.to_uppercase())),
            ("no such tenant", json!("2-16a-no-such-tenant")),
        ];
        for env in [None, Some(SECRET_A)] {
            set_env(env);
            for route in EVENT_ROUTES {
                for (label, tenant) in &mismatches {
                    let o = call(&w, route, Some(SECRET_A), Some(tenant.clone()), "").await;
                    cells.status(
                        format!(
                            "service={with_service} {} | {route:?} | body tenant_id: {label}",
                            env_label(env)
                        ),
                        refused_403(&w, &o),
                        &o,
                    );
                    expected += 1;
                }
            }
        }
        set_env(None);
        let counts = w.counts().await;
        cells.fact(
            format!("service={with_service} | nothing ran"),
            counts == (0, 0),
            format!("runs (A, B) = {counts:?}"),
        );
        expected += 1;
    }
    cells.finish(
        "body_tenant_id_mismatching_the_secrets_tenant_is_refused",
        expected,
    );
}

/// No secret, an empty secret and a blank secret are refused on all three
/// routes with the 2.16e body, whatever the old variable holds, and nothing
/// runs.
#[tokio::test(flavor = "multi_thread")]
async fn no_secret_or_empty_secret_is_refused() {
    let db = named_test_db();
    let _env = lock_env();
    let mut cells = Cells::new();
    let mut expected = 0;
    let headers: [(&str, Option<&str>); 3] = [
        ("no header", None),
        ("empty", Some("")),
        ("blank", Some("   ")),
    ];

    for with_service in [false, true] {
        let w = World::boot(db.clone(), with_service).await;
        for env in [None, Some(""), Some("   "), Some(GLOBAL_SECRET)] {
            set_env(env);
            for route in ALL_ROUTES {
                for (label, secret) in headers {
                    // Body tenant is A's, trigger id is A's: the only thing
                    // missing is a secret.
                    let o = call(&w, route, secret, Some(json!(w.a.id)), &w.a.webhook).await;
                    cells.status(
                        format!(
                            "service={with_service} {env:?} | {route:?} | secret header: {label}"
                        ),
                        refused_401(&w, &o),
                        &o,
                    );
                    expected += 1;
                }
            }
        }
        set_env(None);
        let counts = w.counts().await;
        cells.fact(
            format!("service={with_service} | nothing ran"),
            counts == (0, 0),
            format!("runs (A, B) = {counts:?}"),
        );
        expected += 1;
    }
    cells.finish("no_secret_or_empty_secret_is_refused", expected);
}

/// `POST /webhooks/{trigger_id}` is not a capability URL: the trigger id alone
/// does nothing, for a real trigger, a disabled one and one that does not
/// exist (the last must not be told apart from the others).
#[tokio::test(flavor = "multi_thread")]
async fn webhook_by_trigger_id_alone_is_refused() {
    let db = named_test_db();
    let _env = lock_env();
    let mut cells = Cells::new();
    let mut expected = 0;

    for with_service in [false, true] {
        let w = World::boot(db.clone(), with_service).await;
        set_env(None);
        let unknown = uuid::Uuid::new_v4().to_string();
        let targets: [(&str, &str); 5] = [
            ("A's trigger", &w.a.webhook),
            ("B's trigger", &w.b.webhook),
            ("A's disabled trigger", &w.a.webhook_disabled),
            ("A's document trigger", &w.a.document),
            ("no such trigger", &unknown),
        ];
        for (label, trigger_id) in targets {
            let o = call(&w, Route::Webhook, None, None, trigger_id).await;
            cells.status(
                format!("service={with_service} | trigger id alone | {label}"),
                refused_401(&w, &o),
                &o,
            );
            expected += 1;
        }
        let counts = w.counts().await;
        cells.fact(
            format!("service={with_service} | nothing ran"),
            counts == (0, 0),
            format!("runs (A, B) = {counts:?}"),
        );
        expected += 1;
    }
    cells.finish("webhook_by_trigger_id_alone_is_refused", expected);
}

/// The process-wide `WEBHOOK_SECRET` is no longer a credential: set, and sent
/// in the header, it is refused on all three routes, with the body or the
/// trigger id naming a real tenant (what the old check would have trusted).
#[allow(non_snake_case)]
#[tokio::test(flavor = "multi_thread")]
async fn the_old_global_WEBHOOK_SECRET_alone_is_refused() {
    let db = named_test_db();
    let _env = lock_env();
    let mut cells = Cells::new();
    let mut expected = 0;

    for with_service in [false, true] {
        let w = World::boot(db.clone(), with_service).await;
        set_env(Some(GLOBAL_SECRET));
        for route in ALL_ROUTES {
            for tenant in [&w.a, &w.b] {
                let o = call(
                    &w,
                    route,
                    Some(GLOBAL_SECRET),
                    Some(json!(tenant.id)),
                    &tenant.webhook,
                )
                .await;
                cells.status(
                    format!(
                        "service={with_service} | {route:?} | the global secret, tenant {}",
                        if tenant.id == w.a.id { "A" } else { "B" }
                    ),
                    refused_401(&w, &o),
                    &o,
                );
                expected += 1;
            }
        }
        set_env(None);
        let counts = w.counts().await;
        cells.fact(
            format!("service={with_service} | nothing ran"),
            counts == (0, 0),
            format!("runs (A, B) = {counts:?}"),
        );
        expected += 1;
    }
    cells.finish("the_old_global_WEBHOOK_SECRET_alone_is_refused", expected);
}

/// A wrong secret, and secrets that only look like a tenant's (a prefix, a
/// suffix, another case, padded), are refused on all three routes.
#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_or_lookalike_secret_is_refused_on_all_three_routes() {
    let db = named_test_db();
    let _env = lock_env();
    let mut cells = Cells::new();
    let mut expected = 0;

    let w = World::boot(db, false).await;
    set_env(None);
    let lookalikes: [(&str, String); 6] = [
        ("unknown", WRONG_SECRET.to_string()),
        ("prefix of A's", SECRET_A[..SECRET_A.len() - 1].to_string()),
        ("A's plus a suffix", format!("{SECRET_A}x")),
        ("A's in another case", SECRET_A.to_uppercase()),
        ("A's padded", format!(" {SECRET_A} ")),
        ("A's and B's joined", format!("{SECRET_A}{SECRET_B}")),
    ];
    for route in ALL_ROUTES {
        for (label, secret) in &lookalikes {
            let o = call(&w, route, Some(secret), Some(json!(w.a.id)), &w.a.webhook).await;
            cells.status(
                format!("{route:?} | secret: {label}"),
                refused_401(&w, &o),
                &o,
            );
            expected += 1;
        }
    }
    let counts = w.counts().await;
    cells.fact(
        "nothing ran".to_string(),
        counts == (0, 0),
        format!("runs (A, B) = {counts:?}"),
    );
    expected += 1;
    cells.finish(
        "a_wrong_or_lookalike_secret_is_refused_on_all_three_routes",
        expected,
    );
}

/// The right tenant's secret is accepted, and the tenant comes from the
/// secret: the body `tenant_id` absent works, equal works, and in both cases
/// only that tenant's triggers run, though the other tenant's triggers match
/// the same event. The old variable's state is irrelevant.
#[tokio::test(flavor = "multi_thread")]
async fn the_right_tenant_secret_is_accepted_and_the_tenant_comes_from_it() {
    let db = named_test_db();
    let _env = lock_env();
    let mut cells = Cells::new();
    let mut expected = 0;

    for env in [None, Some(GLOBAL_SECRET)] {
        set_env(env);
        let w = World::boot(db.clone(), false).await;
        for (who, own) in [("A", &w.a), ("B", &w.b)] {
            let own_is_a = own.id == w.a.id;
            let delta = |before: (i64, i64), after: (i64, i64)| {
                let (own_before, other_before) = if own_is_a {
                    (before.0, before.1)
                } else {
                    (before.1, before.0)
                };
                let (own_after, other_after) = if own_is_a {
                    (after.0, after.1)
                } else {
                    (after.1, after.0)
                };
                own_after == own_before + 1 && other_after == other_before
            };
            for route in EVENT_ROUTES {
                for (label, body_tenant) in [
                    ("body tenant_id absent", None),
                    ("body tenant_id equal", Some(json!(own.id))),
                ] {
                    let before = w.counts().await;
                    let o = call(&w, route, Some(own.secret), body_tenant, "").await;
                    let after = w.counts().await;
                    cells.status(
                        format!("{} | {route:?} | {who}'s secret, {label}", env_label(env)),
                        admitted(route, &o) && delta(before, after),
                        &o,
                    );
                    expected += 1;
                }
            }
            let before = w.counts().await;
            let o = call(&w, Route::Webhook, Some(own.secret), None, &own.webhook).await;
            let after = w.counts().await;
            cells.status(
                format!(
                    "{} | Webhook | {who}'s secret on {who}'s trigger",
                    env_label(env)
                ),
                admitted(Route::Webhook, &o) && delta(before, after),
                &o,
            );
            expected += 1;
        }
    }
    set_env(None);
    cells.finish(
        "the_right_tenant_secret_is_accepted_and_the_tenant_comes_from_it",
        expected,
    );
}

/// On `/webhooks/{id}` the ownership check comes first and answers the same
/// for a disabled trigger as for a live one: a tenant's own disabled trigger is
/// `ignored` as before, another tenant's disabled trigger is 403 (not
/// `ignored`, which would say it exists), and an id that does not exist is the
/// 404 it was.
#[tokio::test(flavor = "multi_thread")]
async fn webhook_ownership_is_checked_before_the_trigger_is_described() {
    let db = named_test_db();
    let _env = lock_env();
    let mut cells = Cells::new();

    let w = World::boot(db, false).await;
    set_env(None);

    let o = call(
        &w,
        Route::Webhook,
        Some(SECRET_A),
        None,
        &w.a.webhook_disabled,
    )
    .await;
    cells.status(
        "A's secret on A's disabled trigger".to_string(),
        o.status == StatusCode::OK
            && json_body(&o) == Some(json!({"status": "ignored", "reason": "disabled"})),
        &o,
    );
    let o = call(
        &w,
        Route::Webhook,
        Some(SECRET_A),
        None,
        &w.b.webhook_disabled,
    )
    .await;
    cells.status(
        "A's secret on B's disabled trigger".to_string(),
        refused_403(&w, &o),
        &o,
    );
    let unknown = uuid::Uuid::new_v4().to_string();
    let o = call(&w, Route::Webhook, Some(SECRET_A), None, &unknown).await;
    cells.status(
        "A's secret on a trigger that does not exist".to_string(),
        o.status == StatusCode::NOT_FOUND
            && json_body(&o).is_some_and(|v| v["code"] == "NOT_FOUND"),
        &o,
    );
    let counts = w.counts().await;
    cells.fact(
        "nothing ran".to_string(),
        counts == (0, 0),
        format!("runs (A, B) = {counts:?}"),
    );
    cells.finish(
        "webhook_ownership_is_checked_before_the_trigger_is_described",
        4,
    );
}

/// Neither a secret nor its hash is ever logged, on any path: accepted,
/// refused 401, refused 403, the old variable. The capture also saw the
/// handlers' own lines for this world, so the absence is not an empty log.
#[tokio::test(flavor = "multi_thread")]
async fn no_secret_and_no_hash_is_ever_logged() {
    let db = named_test_db();
    let _env = lock_env();

    let w = World::boot(db, false).await;
    for env in [None, Some(GLOBAL_SECRET)] {
        set_env(env);
        for route in ALL_ROUTES {
            for secret in [
                Some(SECRET_A),
                Some(SECRET_B),
                Some(WRONG_SECRET),
                Some(GLOBAL_SECRET),
                Some(""),
                None,
            ] {
                call(&w, route, secret, Some(json!(w.a.id)), &w.a.webhook).await;
                call(&w, route, secret, Some(json!(w.b.id)), &w.b.webhook).await;
            }
        }
    }
    set_env(None);

    let events = global_capture().0.lock().unwrap().clone();
    let needles: Vec<String> = [SECRET_A, SECRET_B, GLOBAL_SECRET, WRONG_SECRET]
        .iter()
        .flat_map(|s| [s.to_string(), sha256_hex(s)])
        .collect();
    let mut leaks = Vec::new();
    for (level, target, fields) in &events {
        let text = format!("{target} {fields:?}");
        for needle in &needles {
            if text.contains(needle.as_str()) {
                leaks.push(format!("{level} {target}: {fields:?}"));
            }
        }
    }
    eprintln!(
        "LOG CAPTURE | {} events | {} carrying a secret or a hash",
        events.len(),
        leaks.len()
    );
    assert!(
        events.iter().any(|(_, _, fields)| fields
            .iter()
            .any(|(key, value)| key == "trigger_id" && value == &w.a.webhook)),
        "the capture saw no handler line for this world: the check would be vacuous"
    );
    assert!(
        leaks.is_empty(),
        "a secret or a hash reached the log:\n{}",
        leaks.join("\n")
    );
}

/// The owner's provisioning SQL, exactly as it goes in the owner-commands
/// file (`{tenant_id}` and `{secret_sha256}` are the only placeholders). The
/// test runs this text, so the text is known to work: provision, replace,
/// revoke.
const OWNER_PROVISION_SQL: &str = "INSERT INTO tenant_event_secrets (tenant_id, secret_sha256) \
     VALUES ('{tenant_id}', '{secret_sha256}');";
const OWNER_REPLACE_SQL: &str = "UPDATE tenant_event_secrets \
     SET secret_sha256 = '{secret_sha256}', rotated_at = EXTRACT(EPOCH FROM now())::BIGINT \
     WHERE tenant_id = '{tenant_id}';";
const OWNER_REVOKE_SQL: &str = "DELETE FROM tenant_event_secrets WHERE tenant_id = '{tenant_id}';";

fn owner_sql(template: &str, tenant_id: &str, secret: &str) -> String {
    template
        .replace("{tenant_id}", tenant_id)
        .replace("{secret_sha256}", &sha256_hex(secret))
}

/// The owner's provision, replace and revoke statements do what the deploy row
/// says: a provisioned secret works, a replaced one stops the old and starts
/// the new (and stamps `rotated_at`), a revoked one is refused.
#[tokio::test(flavor = "multi_thread")]
async fn the_owner_provisioning_sql_provisions_replaces_and_revokes() {
    let db = named_test_db();
    let _env = lock_env();
    set_env(None);
    let w = World::boot(db, false).await;
    // A third tenant with no secret yet.
    let tenant_db = TenantDb::new(Arc::new(PostgresClient {
        pool: w.pool.clone(),
    }));
    let c = seed_tenant_without_secret(&tenant_db, &w.pool).await;
    const FIRST: &str = "tenant-c-first-secret-dummy-2-16a";
    const SECOND: &str = "tenant-c-second-secret-dummy-2-16a";
    clear_dummy_secrets(&w.pool, &[FIRST, SECOND]).await;
    let mut cells = Cells::new();

    let o = call(&w, Route::Webhook, Some(FIRST), None, &c.webhook).await;
    cells.status(
        "before provisioning: C's secret".to_string(),
        refused_401(&w, &o),
        &o,
    );

    sqlx::query(&owner_sql(OWNER_PROVISION_SQL, &c.id, FIRST))
        .execute(&w.pool)
        .await
        .expect("the owner's provision statement");
    let o = call(&w, Route::Webhook, Some(FIRST), None, &c.webhook).await;
    cells.status(
        "after provisioning: C's secret".to_string(),
        admitted(Route::Webhook, &o),
        &o,
    );

    sqlx::query(&owner_sql(OWNER_REPLACE_SQL, &c.id, SECOND))
        .execute(&w.pool)
        .await
        .expect("the owner's replace statement");
    let o = call(&w, Route::Webhook, Some(FIRST), None, &c.webhook).await;
    cells.status(
        "after replacing: the old secret".to_string(),
        refused_401(&w, &o),
        &o,
    );
    let o = call(&w, Route::Webhook, Some(SECOND), None, &c.webhook).await;
    cells.status(
        "after replacing: the new secret".to_string(),
        admitted(Route::Webhook, &o),
        &o,
    );
    let rotated: Option<i64> =
        sqlx::query_scalar("SELECT rotated_at FROM tenant_event_secrets WHERE tenant_id = $1")
            .bind(&c.id)
            .fetch_one(&w.pool)
            .await
            .expect("read rotated_at");
    cells.fact(
        "after replacing: rotated_at is stamped".to_string(),
        rotated.is_some_and(|t| t > 0),
        format!("rotated_at = {rotated:?}"),
    );

    sqlx::query(&owner_sql(OWNER_REVOKE_SQL, &c.id, SECOND))
        .execute(&w.pool)
        .await
        .expect("the owner's revoke statement");
    let o = call(&w, Route::Webhook, Some(SECOND), None, &c.webhook).await;
    cells.status(
        "after revoking: the new secret".to_string(),
        refused_401(&w, &o),
        &o,
    );
    cells.finish(
        "the_owner_provisioning_sql_provisions_replaces_and_revokes",
        6,
    );
}

/// A tenant with an agent and a webhook trigger, and no secret.
async fn seed_tenant_without_secret(tenant_db: &TenantDb, pool: &PgPool) -> Tenant {
    let tenant = tenant_db
        .create_tenant(
            format!("2-16a-c-{}", uuid::Uuid::new_v4()),
            TenantTier::Free,
        )
        .await
        .expect("seed tenant");
    create_tenant_agent(
        pool,
        &tenant.id,
        CreateTenantAgentRequest {
            agent_name: AGENT.to_string(),
            display_name: "2.16a probe".to_string(),
            description: None,
            config: json!({
                "model": "2-16a-model",
                "system_prompt": "2.16a probe",
                "tools": [],
                "max_tool_iterations": 1,
                "parallel_tools": false
            }),
        },
    )
    .await
    .expect("seed tenant agent");
    let webhook = make_trigger(pool, &tenant.id, "hook", "webhook", json!({}), true).await;
    Tenant {
        id: tenant.id,
        secret: "",
        webhook,
        webhook_disabled: String::new(),
        document: String::new(),
    }
}

/// The table keeps only what the design says: one row per tenant, a hash
/// shared by no two tenants, and nothing that is not a lowercase SHA-256 hex
/// digest (so a plaintext secret pasted by mistake is refused by the database).
#[tokio::test(flavor = "multi_thread")]
async fn the_table_stores_only_a_sha256_one_row_per_tenant_unique() {
    let db = named_test_db();
    let _env = lock_env();
    set_env(None);
    let w = World::boot(db, false).await;
    let insert = |tenant_id: String, value: String| {
        let pool = w.pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO tenant_event_secrets (tenant_id, secret_sha256) VALUES ($1, $2)",
            )
            .bind(tenant_id)
            .bind(value)
            .execute(&pool)
            .await
        }
    };
    let mut cells = Cells::new();
    clear_dummy_secrets(&w.pool, &["tenant-d-secret-dummy-2-16a"]).await;
    let fresh = sha256_hex("tenant-d-secret-dummy-2-16a");
    let tenant_db = TenantDb::new(Arc::new(PostgresClient {
        pool: w.pool.clone(),
    }));
    // D has no row yet: the two cases below can fail only on their own constraint.
    let d = seed_tenant_without_secret(&tenant_db, &w.pool).await;

    // A and B already have a row each (seeded by the world).
    let second_row_for_a = insert(w.a.id.clone(), fresh.clone()).await;
    cells.fact(
        "a second row for the same tenant".to_string(),
        second_row_for_a.is_err(),
        format!("{:?}", second_row_for_a.map(|r| r.rows_affected())),
    );
    let same_hash_for_d = insert(d.id.clone(), sha256_hex(SECRET_A)).await;
    cells.fact(
        "a hash another tenant already has".to_string(),
        same_hash_for_d.is_err(),
        format!("{:?}", same_hash_for_d.map(|r| r.rows_affected())),
    );
    for (label, value) in [
        (
            "a plaintext secret",
            "tenant-d-secret-dummy-2-16a".to_string(),
        ),
        ("upper-case hex", fresh.to_uppercase()),
        ("a hash one character short", fresh[1..].to_string()),
        ("a hash one character long", format!("{fresh}0")),
        ("an empty string", String::new()),
    ] {
        let result = insert(d.id.clone(), value).await;
        cells.fact(
            format!("refused: {label}"),
            result.is_err(),
            format!("{:?}", result.map(|r| r.rows_affected())),
        );
    }
    let ok = insert(d.id.clone(), fresh).await;
    cells.fact(
        "a lowercase SHA-256 hex digest".to_string(),
        ok.is_ok(),
        format!("{:?}", ok.map(|r| r.rows_affected())),
    );
    cells.finish(
        "the_table_stores_only_a_sha256_one_row_per_tenant_unique",
        8,
    );
}
