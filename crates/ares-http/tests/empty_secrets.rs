//! Item 2.16e: an empty secret never authenticates, and an unset
//! `WEBHOOK_SECRET` fails closed.
//!
//! Rulings: `2026-09-29-DELEGATED-sr-2.16-empty-secrets` (decided A), as
//! amended by `2026-09-29-DELEGATED-sr-2.16e-webhook-unset`. Probes E1, E6
//! and E7 are the 2.16 isolation receipt's (Appendix B.6), kept here as
//! regressions.
//!
//! - `ADMIN_API_KEY` empty or whitespace-only is treated exactly as unset, and
//!   an empty or whitespace-only `X-Admin-Secret` never matches: only a
//!   non-empty key with the right header passes the static-secret path.
//! - `WEBHOOK_SECRET` unset, empty or whitespace-only refuses every request
//!   on both `/api/events/*` routes: only a non-empty secret with the right
//!   `X-Webhook-Secret` gets past the check.
//! - A host with both secrets non-empty behaves as before, and the JWT admin
//!   path is untouched.
//!
//! Every test drives the real router in-process: `ares_http::build_router`,
//! which nests `create_router` at `/api` with `admin_middleware` on the admin
//! routes and the two public event routes as mounted. The admitted cells reach
//! real handlers, so the tests need a live scratch Postgres, and they touch
//! only the one `TEST_DATABASE_URL` names (never `ares_test`):
//!
//! - `TEST_DATABASE_URL` unset or empty: every test panics first, before it
//!   touches anything. They never skip, and never fall back to
//!   `DATABASE_URL`, a dotenv file or the unix-socket `ares_test` (the
//!   fallbacks of `ares_test_support::test_db_url`, which no test here
//!   calls).
//! - Set but unreachable: every test panics (the gate in `tests/common`).
//! - Neither panic prints the URL.
//!
//! `ADMIN_API_KEY` and `WEBHOOK_SECRET` are process-global. Every test takes
//! the one static [`ENV_LOCK`] for its whole body and sets or removes the
//! variable before each request. The secrets are dummies spelled in this
//! file; no test reads a real one.

#![cfg(feature = "postgres")]
// Deliberate: each test holds `ENV_LOCK` across its awaited requests,
// because the code under test reads the variables mid-await (the same
// reasoning as `ADMIN_ENV_LOCK` in `admin/shared.rs`).
#![allow(clippy::await_holding_lock)]

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ares_http::auth::jwks::{encode_token_with_pem_key, IssuerClaims, JwksCache, RoleEntry};
use ares_http::auth::jwt::AuthService;
use ares_store::TenantDb;
use ares_types::models::TenantTier;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

const ADMIN_ENV: &str = "ADMIN_API_KEY";
const WEBHOOK_ENV: &str = "WEBHOOK_SECRET";

/// Dummy secrets for this file only. They protect nothing.
const ADMIN_TEST_SECRET: &str = "s3cret-test";
const WEBHOOK_TEST_SECRET: &str = "hook-test";
const WRONG_TEST_SECRET: &str = "wrong-secret-test";
const TEST_JWT_SECRET: &str = "empty-secrets-test-jwt-secret-at-least-32-chars";

/// The admin refusal, byte for byte as at `c0b648f` (`admin.rs:228-235`).
const ADMIN_REFUSAL: &str =
    r#"{"error":"Admin access requires X-Admin-Secret header or JWT with admin role"}"#;

/// The two event routes, as `build_router` mounts them.
const EVENT_ROUTES: [&str; 2] = ["/api/events/document-upload", "/api/events/field-change"];

/// A tenant with no row and no triggers: an admitted event writes nothing.
const NO_SUCH_TENANT: &str = "2-16e-test-no-such-tenant";

/// The one lock that serialises every mutation of the two variables in this
/// binary. Poisoning is recovered: each test sets what it needs first.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_env() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Set (`Some`) or remove (`None`) one variable. The caller holds [`ENV_LOCK`].
fn set_env(name: &str, value: Option<&str>) {
    match value {
        Some(v) => std::env::set_var(name, v),
        None => std::env::remove_var(name),
    }
}

/// The webhook refusal as at `c0b648f`: `AppError::Auth("Invalid webhook
/// secret")` through `app_error_into_response`, status 401.
fn webhook_refusal() -> serde_json::Value {
    serde_json::json!({
        "error": "Authentication error: Invalid webhook secret",
        "code": "AUTHENTICATION_FAILED",
    })
}

/// The four configured states of a secret: unset, empty, whitespace-only, set.
fn configured_states(secret: &'static str) -> [(&'static str, Option<&'static str>); 4] {
    [
        ("unset", None),
        ("empty", Some("")),
        ("blank", Some("   ")),
        ("set", Some(secret)),
    ]
}

/// What a request carries in the secret header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Given {
    Missing,
    Empty,
    Blank,
    Wrong,
    /// The configured value, or the dummy secret when the variable is unset.
    Right,
}

impl Given {
    fn value(self, configured: Option<&'static str>, dummy: &'static str) -> Option<&'static str> {
        match self {
            Given::Missing => None,
            Given::Empty => Some(""),
            Given::Blank => Some("   "),
            Given::Wrong => Some(WRONG_TEST_SECRET),
            Given::Right => Some(configured.unwrap_or(dummy)),
        }
    }
}

// ---------------------------------------------------------------------------
// The router under test
// ---------------------------------------------------------------------------

struct Harness {
    router: axum::Router,
    /// A tenant seeded for this test; an admitted `GET /api/admin/tenants`
    /// lists it.
    tenant_id: String,
}

/// The one database this binary may touch: the one `TEST_DATABASE_URL` names.
///
/// Every test calls this first, before anything else. Unset, empty or not
/// valid Unicode panics: these security tests never skip, and never fall back
/// to another database. The message names the variable, never a URL.
fn named_test_db() -> String {
    match std::env::var(common::DB_ENV) {
        Ok(url) if !url.trim().is_empty() => url,
        _ => panic!(
            "{test}: {var} is unset or empty. The empty-secret tests never skip and never fall \
             back to another database: set {var} to a scratch database (never ares_test).",
            test = common::current_test_name(),
            var = common::DB_ENV,
        ),
    }
}

/// Migrations run once per test binary, on the named database.
static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// `build_router` over the database [`named_test_db`] returned, with one
/// seeded tenant. `jwks_url` points the admin JWT path at a local issuer key
/// set.
async fn boot(db_url: String, jwks_url: Option<String>) -> Harness {
    let test = common::current_test_name();
    // Configured and unreachable panics here, naming the variable only.
    let common::Gate::Run(db_url) = common::gate(&test, true, db_url).await else {
        unreachable!("a configured gate never skips");
    };
    // A pool on exactly that URL: no resolver, no dotenv, no fallback.
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
    let pg = Arc::new(ares_store::PostgresClient { pool });
    let tenant_db = Arc::new(TenantDb::new(pg));
    let tenant = tenant_db
        .create_tenant(
            format!("2-16e-test-{}", uuid::Uuid::new_v4()),
            TenantTier::Free,
        )
        .await
        .expect("seed tenant");

    let mut auth = AuthService::new(TEST_JWT_SECRET.to_string(), 900, 604_800);
    if let Some(url) = jwks_url {
        auth = auth.with_jwks(Arc::new(JwksCache::new(url)));
    }
    let ctx = cordis::Context::new_root();
    ctx.provide_arc(tenant_db);
    ctx.provide_arc(Arc::new(auth));
    Harness {
        router: ares_http::build_router(ctx),
        tenant_id: tenant.id,
    }
}

async fn send(router: &axum::Router, req: Request<Body>) -> (StatusCode, String, String) {
    let response = router.clone().oneshot(req).await.expect("router");
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        content_type,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn admin_get(secret: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri("/api/admin/tenants");
    if let Some(value) = secret {
        builder = builder.header("x-admin-secret", value);
    }
    builder.body(Body::empty()).expect("request")
}

fn admin_get_with_token(token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri("/api/admin/tenants")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request")
}

fn event_post(route: &str, secret: Option<&str>) -> Request<Body> {
    let body = if route.ends_with("/document-upload") {
        serde_json::json!({
            "tenant_id": NO_SUCH_TENANT,
            "bucket": "2-16e-test-bucket",
            "key": "2-16e-test/key.pdf",
        })
    } else {
        serde_json::json!({
            "tenant_id": NO_SUCH_TENANT,
            "table": "t",
            "column": "c",
            "record_id": "r",
            "old_value": 1,
            "new_value": 2,
        })
    };
    let mut builder = Request::builder()
        .method("POST")
        .uri(route)
        .header("content-type", "application/json");
    if let Some(value) = secret {
        builder = builder.header("x-webhook-secret", value);
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

/// An admitted `GET /api/admin/tenants`: 200, and the handler ran (the
/// seeded tenant is listed).
fn admin_admitted(h: &Harness, status: StatusCode, body: &str) -> bool {
    status == StatusCode::OK
        && serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.as_array().cloned())
            .is_some_and(|rows| rows.iter().any(|r| r["id"] == h.tenant_id.as_str()))
}

/// The admin refusal: 401, JSON, and today's body.
fn admin_refused(status: StatusCode, content_type: &str, body: &str) -> bool {
    status == StatusCode::UNAUTHORIZED
        && content_type == "application/json"
        && body == ADMIN_REFUSAL
}

/// The webhook refusal: 401 and today's body.
fn webhook_refused(status: StatusCode, body: &str) -> bool {
    status == StatusCode::UNAUTHORIZED
        && serde_json::from_str::<serde_json::Value>(body).ok() == Some(webhook_refusal())
}

// ---------------------------------------------------------------------------
// Admin: the static-secret path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_secret_matrix() {
    let db = named_test_db();
    let _env = lock_env();
    set_env(WEBHOOK_ENV, None);
    let h = boot(db, None).await;

    let headers = [
        Given::Missing,
        Given::Empty,
        Given::Blank,
        Given::Wrong,
        Given::Right,
    ];
    let (mut cells, mut admitted, mut wrong) = (0usize, 0usize, Vec::new());
    for (state, configured) in configured_states(ADMIN_TEST_SECRET) {
        for given in headers {
            set_env(ADMIN_ENV, configured);
            let (status, content_type, body) = send(
                &h.router,
                admin_get(given.value(configured, ADMIN_TEST_SECRET)),
            )
            .await;
            let admit = configured == Some(ADMIN_TEST_SECRET) && given == Given::Right;
            let ok = if admit {
                admin_admitted(&h, status, &body)
            } else {
                admin_refused(status, &content_type, &body)
            };
            let expect = if admit { "admit" } else { "401" };
            eprintln!(
                "CELL admin | ADMIN_API_KEY {state} | X-Admin-Secret {given:?} | status {} | expect {expect} | {}",
                status.as_u16(),
                if ok { "ok" } else { "WRONG" }
            );
            if !ok {
                wrong.push(format!(
                    "ADMIN_API_KEY {state}, X-Admin-Secret {given:?}: status {} body {body:?}, expected {expect}",
                    status.as_u16()
                ));
            }
            cells += 1;
            admitted += usize::from(admit);
        }
    }
    set_env(ADMIN_ENV, None);

    eprintln!(
        "CELLS admin_secret_matrix | {cells} cells | {admitted} admit | {} refuse",
        cells - admitted
    );
    assert_eq!(cells, 20, "4 configured states x 5 headers");
    assert_eq!(admitted, 1, "only the non-empty key with the right header");
    assert!(
        wrong.is_empty(),
        "{} of {cells} cells wrong:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// Probe E1 (receipt B.6): `ADMIN_API_KEY` set empty and an empty
/// `X-Admin-Secret` was admitted as actor `admin_secret`.
#[tokio::test]
async fn e1_empty_admin_secret_and_empty_header_is_refused() {
    let db = named_test_db();
    let _env = lock_env();
    let h = boot(db, None).await;

    set_env(ADMIN_ENV, Some(""));
    let (status, content_type, body) = send(&h.router, admin_get(Some(""))).await;
    set_env(ADMIN_ENV, None);

    eprintln!(
        "PROBE E1 | ADMIN_API_KEY set-empty, X-Admin-Secret empty | status {} | body {body:?}",
        status.as_u16()
    );
    assert_eq!(status, StatusCode::UNAUTHORIZED, "E1 must be refused");
    assert_eq!(content_type, "application/json");
    assert_eq!(body, ADMIN_REFUSAL);
}

/// Send one raw `GET /api/admin/tenants` over a real socket, with
/// `header_line` (CRLF-terminated) verbatim. Returns the status line and body.
async fn raw_admin_get(addr: SocketAddr, header_line: &str) -> (String, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut sock = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let request = format!(
        "GET /api/admin/tenants HTTP/1.1\r\nHost: empty-secrets-test\r\n{header_line}Connection: close\r\n\r\n"
    );
    sock.write_all(request.as_bytes()).await.expect("write");
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), sock.read_to_end(&mut buf))
        .await
        .expect("response within 10 s")
        .expect("read");
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status_line = text.lines().next().unwrap_or("").to_string();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status_line, body)
}

/// Probe E6 (receipt B.6), the wire form of E1: hyper parses a header line
/// `X-Admin-Secret:` with an empty value (`curl -H 'X-Admin-Secret:'` would
/// drop the header, so the request is written by hand).
#[tokio::test]
async fn e6_empty_header_on_the_wire_is_refused() {
    let db = named_test_db();
    let _env = lock_env();
    let h = boot(db, None).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let served = h.router.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, served).await;
    });

    set_env(ADMIN_ENV, Some(""));
    let (status_line, body) = raw_admin_get(addr, "X-Admin-Secret:\r\n").await;
    eprintln!("PROBE E6 | real socket, ADMIN_API_KEY set-empty, raw `X-Admin-Secret:` | {status_line:?} | body {body:?}");
    assert!(
        status_line.starts_with("HTTP/1.1 401"),
        "E6 must be refused, got {status_line:?}"
    );
    assert_eq!(body, ADMIN_REFUSAL);

    // Control: the same socket path admits the right secret when the key is
    // non-empty, so the 401 above is the middleware's refusal.
    set_env(ADMIN_ENV, Some(ADMIN_TEST_SECRET));
    let (status_line, _) =
        raw_admin_get(addr, &format!("X-Admin-Secret: {ADMIN_TEST_SECRET}\r\n")).await;
    set_env(ADMIN_ENV, None);
    eprintln!("PROBE E6 control | real socket, ADMIN_API_KEY set, right header | {status_line:?}");
    assert!(
        status_line.starts_with("HTTP/1.1 200"),
        "control must be admitted, got {status_line:?}"
    );
}

// ---------------------------------------------------------------------------
// Webhook: the `/api/events/*` check
// ---------------------------------------------------------------------------

#[tokio::test]
async fn webhook_secret_matrix() {
    let db = named_test_db();
    let _env = lock_env();
    set_env(ADMIN_ENV, None);
    let h = boot(db, None).await;

    let headers = [Given::Missing, Given::Empty, Given::Wrong, Given::Right];
    let (mut cells, mut admitted, mut wrong) = (0usize, 0usize, Vec::new());
    for route in EVENT_ROUTES {
        for (state, configured) in configured_states(WEBHOOK_TEST_SECRET) {
            for given in headers {
                set_env(WEBHOOK_ENV, configured);
                let (status, _, body) = send(
                    &h.router,
                    event_post(route, given.value(configured, WEBHOOK_TEST_SECRET)),
                )
                .await;
                let admit = configured == Some(WEBHOOK_TEST_SECRET) && given == Given::Right;
                let ok = if admit {
                    status == StatusCode::OK
                } else {
                    webhook_refused(status, &body)
                };
                let expect = if admit { "admit" } else { "401" };
                eprintln!(
                    "CELL webhook | {route} | WEBHOOK_SECRET {state} | X-Webhook-Secret {given:?} | status {} | expect {expect} | {}",
                    status.as_u16(),
                    if ok { "ok" } else { "WRONG" }
                );
                if !ok {
                    wrong.push(format!(
                        "{route}, WEBHOOK_SECRET {state}, X-Webhook-Secret {given:?}: status {} body {body:?}, expected {expect}",
                        status.as_u16()
                    ));
                }
                cells += 1;
                admitted += usize::from(admit);
            }
        }
    }
    set_env(WEBHOOK_ENV, None);

    eprintln!(
        "CELLS webhook_secret_matrix | {cells} cells | {admitted} admit | {} refuse",
        cells - admitted
    );
    assert_eq!(cells, 32, "2 routes x 4 configured states x 4 headers");
    assert_eq!(
        admitted, 2,
        "only the non-empty secret with the right header, per route"
    );
    assert!(
        wrong.is_empty(),
        "{} of {cells} cells wrong:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// Probe E7 (receipt B.6): `WEBHOOK_SECRET` set empty skipped the check.
/// Every request is now refused, on both routes.
#[tokio::test]
async fn e7_empty_webhook_secret_refuses_every_request() {
    let db = named_test_db();
    let _env = lock_env();
    let h = boot(db, None).await;

    set_env(WEBHOOK_ENV, Some(""));
    let mut wrong = Vec::new();
    for route in EVENT_ROUTES {
        for (label, secret) in [
            ("no header", None),
            ("empty header", Some("")),
            ("any header", Some(WRONG_TEST_SECRET)),
        ] {
            let (status, _, body) = send(&h.router, event_post(route, secret)).await;
            eprintln!("PROBE E7 | {route} | WEBHOOK_SECRET set-empty, {label} | status {} | body {body:?}", status.as_u16());
            if !webhook_refused(status, &body) {
                wrong.push(format!(
                    "{route}, {label}: status {} body {body:?}",
                    status.as_u16()
                ));
            }
        }
    }
    set_env(WEBHOOK_ENV, None);

    assert!(
        wrong.is_empty(),
        "E7 must refuse every request:\n{}",
        wrong.join("\n")
    );
}

// ---------------------------------------------------------------------------
// A correctly configured host
// ---------------------------------------------------------------------------

/// PKCS#8 PEM for the Ed25519 key derived from `seed` (the crate's
/// `auth::test_keys::pem_for_seed`, which is private to its unit tests).
fn pem_for_seed(seed: u8) -> Vec<u8> {
    use base64::Engine;
    use ed25519_dalek::pkcs8::EncodePrivateKey;

    let signing = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let der = signing.to_pkcs8_der().expect("PKCS#8 encode");
    let body = base64::engine::general_purpose::STANDARD.encode(der.as_bytes());
    let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
    for chunk in body.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        pem.push('\n');
    }
    pem.push_str("-----END PRIVATE KEY-----\n");
    pem.into_bytes()
}

/// Serve a JWKS document holding the public half of `seed`'s key on a
/// loopback port (the crate's `auth::test_keys::{jwks_json, jwks_stub}`).
async fn jwks_stub(seed: u8, kid: &str) -> String {
    use base64::Engine;

    let signing = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let x =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes());
    let doc = serde_json::json!({
        "keys": [{"kty": "OKP", "crv": "Ed25519", "x": x, "alg": "EdDSA", "use": "sig", "kid": kid}]
    })
    .to_string();
    let app = axum::Router::new().route(
        "/.well-known/jwks.json",
        axum::routing::get(move || {
            let doc = doc.clone();
            async move {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    doc,
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind jwks stub");
    let addr = listener.local_addr().expect("stub addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/.well-known/jwks.json")
}

/// An issuer token with role `admin` in product `ares`.
fn eddsa_admin_token(seed: u8, kid: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    let claims = IssuerClaims {
        sub: "2-16e-test-admin".into(),
        email: "2-16e-test-admin@example.com".into(),
        exp: now + 3600,
        iat: now,
        roles: Some(std::collections::HashMap::from([(
            "ares".to_string(),
            vec![RoleEntry {
                role: "admin".into(),
                resource_id: None,
            }],
        )])),
        token_version: Some(0),
    };
    encode_token_with_pem_key(&claims, &pem_for_seed(seed), Some(kid)).expect("sign EdDSA")
}

#[tokio::test]
async fn correctly_configured_host_behaves_as_before() {
    let db = named_test_db();
    let _env = lock_env();
    let (seed, kid) = (41u8, "2-16e-test-kid");
    let jwks_url = jwks_stub(seed, kid).await;
    let h = boot(db, Some(jwks_url)).await;

    set_env(ADMIN_ENV, Some(ADMIN_TEST_SECRET));
    set_env(WEBHOOK_ENV, Some(WEBHOOK_TEST_SECRET));

    // Admin, static secret: the right one admits, a wrong one refuses.
    let (status, _, body) = send(&h.router, admin_get(Some(ADMIN_TEST_SECRET))).await;
    eprintln!(
        "CONFIGURED admin | right X-Admin-Secret | status {}",
        status.as_u16()
    );
    assert!(
        admin_admitted(&h, status, &body),
        "right secret must admit: {status} {body}"
    );
    let (status, content_type, body) = send(&h.router, admin_get(Some(WRONG_TEST_SECRET))).await;
    eprintln!(
        "CONFIGURED admin | wrong X-Admin-Secret | status {}",
        status.as_u16()
    );
    assert!(
        admin_refused(status, &content_type, &body),
        "wrong secret must refuse: {status} {body}"
    );

    // Admin, JWT: an EdDSA issuer token with an admin role, no secret header.
    let token = eddsa_admin_token(seed, kid);
    let (status, _, body) = send(&h.router, admin_get_with_token(&token)).await;
    eprintln!(
        "CONFIGURED admin | EdDSA admin JWT, no X-Admin-Secret | status {}",
        status.as_u16()
    );
    assert!(
        admin_admitted(&h, status, &body),
        "EdDSA admin token must admit: {status} {body}"
    );

    // Webhook: the right secret gets past the check, a wrong one is refused.
    for route in EVENT_ROUTES {
        let (status, _, body) = send(&h.router, event_post(route, Some(WEBHOOK_TEST_SECRET))).await;
        eprintln!(
            "CONFIGURED webhook | {route} | right X-Webhook-Secret | status {}",
            status.as_u16()
        );
        assert_eq!(status, StatusCode::OK, "{route}: right secret: {body}");
        let (status, _, body) = send(&h.router, event_post(route, Some(WRONG_TEST_SECRET))).await;
        eprintln!(
            "CONFIGURED webhook | {route} | wrong X-Webhook-Secret | status {}",
            status.as_u16()
        );
        assert!(
            webhook_refused(status, &body),
            "{route}: wrong secret must refuse: {status} {body}"
        );
    }

    set_env(ADMIN_ENV, None);
    set_env(WEBHOOK_ENV, None);
}
