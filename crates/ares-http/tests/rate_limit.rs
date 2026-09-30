//! The client limiter (CR-3, round A): `ares_http::middleware::rate_limit`,
//! driven through ARES's real router (`ares_http::build_router`).
//!
//! Pure: no database, no network. Time comes from a `ManualClock`, so no
//! test sleeps. Addresses come only from the documentation ranges
//! (192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24) and loopback. Keys are
//! dummies.

#![cfg(feature = "postgres")]

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ares_http::config::{RateLimitConfig, ServerConfig};
use ares_http::middleware::rate_limit::{
    legacy_keys_warning, warn_legacy_keys, ManualClock, RateLimitLayer, RefusalCounts,
    EXEMPT_HEALTH_PATHS,
};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use tower::ServiceExt;

// ============================================================================
// Harness
// ============================================================================

fn off() -> RateLimitConfig {
    RateLimitConfig::default()
}

fn ip(s: &str) -> IpAddr {
    s.parse().expect("test IP literal")
}

/// ARES's real router (`build_router`, which serves `/health`) plus one API
/// route, with the limiter layered on top the way `main.rs` layers it.
struct Harness {
    router: Router,
    layer: RateLimitLayer,
    clock: Arc<ManualClock>,
}

impl Harness {
    fn new(config: RateLimitConfig) -> Self {
        let clock = Arc::new(ManualClock::new());
        let layer = RateLimitLayer::with_clock(config, clock.clone());
        Self::with_layer(layer, clock)
    }

    fn with_layer(layer: RateLimitLayer, clock: Arc<ManualClock>) -> Self {
        let router = ares_http::build_router(cordis::Context::new_root())
            .merge(Router::new().route("/v1/ping", get(|| async { "pong" })))
            .layer(layer.clone());
        Self {
            router,
            layer,
            clock,
        }
    }

    /// One request from socket peer `peer`, as `into_make_service_with_connect_info`
    /// delivers it (the peer lands in `ConnectInfo<SocketAddr>`).
    async fn call(
        &self,
        method: Method,
        path: &str,
        peer: &str,
        headers: &[(&str, &str)],
    ) -> Response {
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let mut req = builder.body(Body::empty()).expect("request");
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(ip(peer), 40_000)));
        self.router.clone().oneshot(req).await.expect("infallible")
    }

    async fn get(&self, path: &str, peer: &str, headers: &[(&str, &str)]) -> StatusCode {
        self.call(Method::GET, path, peer, headers).await.status()
    }

    fn refusals(&self) -> RefusalCounts {
        self.layer.limiter().refusals()
    }

    fn client_of(&self, peer: &str, headers: &[(&str, &str)]) -> Option<IpAddr> {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
                HeaderValue::from_str(value).expect("header value"),
            );
        }
        self.layer.limiter().client_ip(Some(ip(peer)), &map)
    }
}

fn retry_after(resp: &Response) -> String {
    resp.headers()
        .get(header::RETRY_AFTER)
        .expect("a 429 carries Retry-After")
        .to_str()
        .expect("ascii")
        .to_string()
}

// ============================================================================
// Log capture: a minimal subscriber that records every event and span field.
// ============================================================================

#[derive(Clone, Default)]
struct Capture {
    lines: Arc<Mutex<Vec<(tracing::Level, String)>>>,
    next_id: Arc<AtomicU64>,
}

impl Capture {
    fn lines(&self) -> Vec<(tracing::Level, String)> {
        self.lines.lock().expect("capture lock").clone()
    }

    fn warn_lines(&self) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|(level, _)| *level == tracing::Level::WARN)
            .map(|(_, line)| line)
            .collect()
    }

    fn all_text(&self) -> String {
        self.lines()
            .into_iter()
            .map(|(_, line)| line)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn push(&self, level: tracing::Level, line: String) {
        self.lines.lock().expect("capture lock").push((level, line));
    }
}

struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        let _ = write!(self.0, " {}={:?}", field.name(), value);
    }
}

impl tracing::Subscriber for Capture {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::always()
    }

    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut fields = Fields(format!("span {}", attrs.metadata().name()));
        attrs.record(&mut fields);
        self.push(*attrs.metadata().level(), fields.0);
        tracing::span::Id::from_u64(self.next_id.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn record(&self, _: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        let mut fields = Fields(String::from("record"));
        values.record(&mut fields);
        self.push(tracing::Level::TRACE, fields.0);
    }

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let meta = event.metadata();
        let mut fields = Fields(format!("{} {}", meta.level(), meta.target()));
        event.record(&mut fields);
        self.push(*meta.level(), fields.0);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

// ============================================================================
// Off by default, and the units in the names
// ============================================================================

#[tokio::test]
async fn off_by_default_no_request_is_ever_refused() {
    let config = ServerConfig::default().rate_limit;
    assert_eq!(config.global_requests_per_minute, 0);
    assert_eq!(config.per_client_requests_per_minute, 0);
    assert_eq!(config.per_key_requests_per_minute, 0);
    assert_eq!(config.burst, 0);
    assert_eq!(config.trusted_proxies, vec![ip("127.0.0.1"), ip("::1")]);

    let h = Harness::new(config);
    assert!(!h.layer.is_enabled(), "every limit at 0 means the limiter is off");

    for n in 0..300 {
        let status = h
            .get(
                "/v1/ping",
                "203.0.113.7",
                &[("authorization", "Bearer ares_dummy_limiter_key_off_0000")],
            )
            .await;
        assert_eq!(status, StatusCode::OK, "request {n} was refused while off");
        let status = h
            .get("/v1/ping", "127.0.0.1", &[("x-forwarded-for", "198.51.100.9")])
            .await;
        assert_eq!(status, StatusCode::OK, "proxied request {n} was refused while off");
    }
    assert_eq!(h.refusals(), RefusalCounts::default());
}

#[test]
fn config_table_names_carry_their_units() {
    let server: ServerConfig = toml::from_str(
        r#"
host = "127.0.0.1"

[rate_limit]
global_requests_per_minute = 600
per_client_requests_per_minute = 60
per_key_requests_per_minute = 120
burst = 5
trusted_proxies = ["127.0.0.1", "::1", "192.0.2.10"]
"#,
    )
    .expect("[rate_limit] parses inside ServerConfig");
    let rl = &server.rate_limit;
    assert_eq!(rl.global_requests_per_minute, 600);
    assert_eq!(rl.per_client_requests_per_minute, 60);
    assert_eq!(rl.per_key_requests_per_minute, 120);
    assert_eq!(rl.burst, 5);
    assert_eq!(
        rl.trusted_proxies,
        vec![ip("127.0.0.1"), ip("::1"), ip("192.0.2.10")]
    );
    assert!(rl.any_limit_set());

    // No table at all: every limit off.
    let bare: ServerConfig = toml::from_str("host = \"127.0.0.1\"\n").expect("bare server");
    assert_eq!(bare.rate_limit, RateLimitConfig::default());
    assert!(!bare.rate_limit.any_limit_set());

    // A key without its unit is refused at load, never silently ignored.
    let unitless = toml::from_str::<ServerConfig>("[rate_limit]\nper_client = 60\n");
    assert!(unitless.is_err(), "an unknown [rate_limit] key must not load");

    // A trusted proxy that is not an IP address is refused at load.
    let bad_proxy = toml::from_str::<ServerConfig>("[rate_limit]\ntrusted_proxies = [\"caddy\"]\n");
    assert!(bad_proxy.is_err(), "a non-IP trusted proxy must not load");
}

#[tokio::test]
async fn units_per_client_60_per_minute_burst_5() {
    let h = Harness::new(RateLimitConfig {
        per_client_requests_per_minute: 60,
        burst: 5,
        ..off()
    });
    assert!(h.layer.is_enabled());

    for n in 1..=5 {
        assert_eq!(
            h.get("/v1/ping", "203.0.113.7", &[]).await,
            StatusCode::OK,
            "request {n} of the burst of 5"
        );
    }
    let sixth = h.call(Method::GET, "/v1/ping", "203.0.113.7", &[]).await;
    assert_eq!(sixth.status(), StatusCode::TOO_MANY_REQUESTS, "the 6th in a burst of 5");
    assert_eq!(retry_after(&sixth), "1", "60 per minute refills one request per second");

    // Another client is untouched.
    assert_eq!(h.get("/v1/ping", "203.0.113.8", &[]).await, StatusCode::OK);

    h.clock.advance(Duration::from_secs(1));
    assert_eq!(
        h.get("/v1/ping", "203.0.113.7", &[]).await,
        StatusCode::OK,
        "1 s later one request (60/min) is admitted"
    );
    assert_eq!(
        h.get("/v1/ping", "203.0.113.7", &[]).await,
        StatusCode::TOO_MANY_REQUESTS,
        "and only one"
    );

    h.clock.advance(Duration::from_millis(500));
    assert_eq!(
        h.get("/v1/ping", "203.0.113.7", &[]).await,
        StatusCode::TOO_MANY_REQUESTS,
        "half a second refills half a request: still refused"
    );
    h.clock.advance(Duration::from_millis(500));
    assert_eq!(h.get("/v1/ping", "203.0.113.7", &[]).await, StatusCode::OK);

    assert_eq!(
        h.refusals(),
        RefusalCounts {
            global: 0,
            client: 3,
            key: 0
        }
    );
}

// ============================================================================
// The client: trusted proxies, spoofing, several hops
// ============================================================================

#[tokio::test]
async fn trusted_proxy_forwarded_for_names_the_client() {
    let h = Harness::new(RateLimitConfig {
        per_client_requests_per_minute: 60,
        burst: 2,
        ..off()
    });
    assert_eq!(
        h.client_of("127.0.0.1", &[("x-forwarded-for", "203.0.113.7")]),
        Some(ip("203.0.113.7"))
    );

    // Two clients behind the same proxy have separate buckets.
    let a = [("x-forwarded-for", "203.0.113.7")];
    let b = [("x-forwarded-for", "203.0.113.8")];
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &a).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &a).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &a).await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &b).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &b).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &b).await, StatusCode::TOO_MANY_REQUESTS);

    // The proxy's own requests (no forwarding header) are a third client.
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &[]).await, StatusCode::OK);

    // ::1 and the IPv4-mapped loopback are the same trusted proxy.
    assert_eq!(h.get("/v1/ping", "::1", &a).await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        h.get("/v1/ping", "::ffff:127.0.0.1", &a).await,
        StatusCode::TOO_MANY_REQUESTS
    );

    // X-Real-IP names the client when there is no X-Forwarded-For.
    let real = [("x-real-ip", "198.51.100.20")];
    assert_eq!(h.client_of("127.0.0.1", &real), Some(ip("198.51.100.20")));
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &real).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &real).await, StatusCode::OK);
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &[("x-forwarded-for", "198.51.100.20")])
            .await,
        StatusCode::TOO_MANY_REQUESTS,
        "X-Real-IP and X-Forwarded-For naming one address are one client"
    );
    // With both headers, X-Forwarded-For wins.
    assert_eq!(
        h.client_of(
            "127.0.0.1",
            &[("x-forwarded-for", "203.0.113.30"), ("x-real-ip", "198.51.100.20")]
        ),
        Some(ip("203.0.113.30"))
    );
}

/// The spoof-resistance test: a client-supplied `X-Forwarded-For` from an
/// untrusted peer is never read.
#[tokio::test]
async fn spoofed_forwarded_for_from_untrusted_peer_is_never_read() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());

    let h = Harness::new(RateLimitConfig {
        per_client_requests_per_minute: 60,
        burst: 2,
        ..off()
    });
    let spoof = [("x-forwarded-for", "203.0.113.7")];

    let via_proxy = h.client_of("127.0.0.1", &spoof);
    let via_untrusted = h.client_of("198.51.100.9", &spoof);
    eprintln!("spoof: peer 127.0.0.1    + X-Forwarded-For 203.0.113.7 -> client {via_proxy:?}");
    eprintln!("spoof: peer 198.51.100.9 + X-Forwarded-For 203.0.113.7 -> client {via_untrusted:?}");
    assert_eq!(via_proxy, Some(ip("203.0.113.7")));
    assert_eq!(via_untrusted, Some(ip("198.51.100.9")), "the header from an untrusted peer is ignored");

    // The untrusted peer spends its own bucket, whatever it claims.
    assert_eq!(h.get("/v1/ping", "198.51.100.9", &spoof).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "198.51.100.9", &spoof).await, StatusCode::OK);
    let mut statuses = Vec::new();
    for headers in [
        vec![("x-forwarded-for", "203.0.113.7")],
        vec![("x-forwarded-for", "203.0.113.8")],
        vec![("x-real-ip", "203.0.113.9")],
        vec![("x-forwarded-for", "203.0.113.10, 127.0.0.1")],
    ] {
        let status = h.get("/v1/ping", "198.51.100.9", &headers).await;
        eprintln!("spoof: peer 198.51.100.9 + {headers:?} -> {status}");
        statuses.push(status);
    }
    assert!(
        statuses.iter().all(|s| *s == StatusCode::TOO_MANY_REQUESTS),
        "rotating the claimed address must not buy a new bucket: {statuses:?}"
    );

    // The claimed address's own bucket was never touched.
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &spoof).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &spoof).await, StatusCode::OK);

    assert_eq!(
        h.refusals(),
        RefusalCounts {
            global: 0,
            client: 4,
            key: 0
        }
    );

    // The refusal lines name the peer's /24, never a full address.
    let warns = capture.warn_lines();
    eprintln!("spoof: refusal log lines: {warns:#?}");
    assert_eq!(warns.len(), 4, "one warn line per refusal: {warns:#?}");
    for line in &warns {
        assert!(line.contains("client"), "the line names the dimension: {line}");
        assert!(line.contains("198.51.100.0/24"), "the line carries the /24: {line}");
    }
    let text = capture.all_text();
    for full in ["198.51.100.9", "203.0.113.7", "203.0.113.8", "203.0.113.9"] {
        assert!(!text.contains(full), "a full address was logged: {full}\n{text}");
    }
}

#[tokio::test]
async fn forwarded_for_with_several_hops_picks_the_rightmost_untrusted() {
    let h = Harness::new(RateLimitConfig {
        per_client_requests_per_minute: 60,
        burst: 1,
        trusted_proxies: vec![
            ip("127.0.0.1"),
            ip("::1"),
            ip("192.0.2.10"),
            ip("192.0.2.11"),
        ],
        ..off()
    });

    let hops = [(
        "x-forwarded-for",
        "198.51.100.77, 203.0.113.9, 192.0.2.10",
    )];
    assert_eq!(h.client_of("127.0.0.1", &hops), Some(ip("203.0.113.9")));
    // Several header lines are one list, in order.
    assert_eq!(
        h.client_of(
            "127.0.0.1",
            &[
                ("x-forwarded-for", "198.51.100.77"),
                ("x-forwarded-for", "203.0.113.9, 192.0.2.11, 192.0.2.10"),
            ]
        ),
        Some(ip("203.0.113.9"))
    );
    assert_eq!(
        h.client_of("127.0.0.1", &[("x-forwarded-for", "203.0.113.9,192.0.2.10")]),
        Some(ip("203.0.113.9"))
    );
    // Every hop trusted: the peer is the client.
    assert_eq!(
        h.client_of("127.0.0.1", &[("x-forwarded-for", "192.0.2.11, 192.0.2.10")]),
        Some(ip("127.0.0.1"))
    );
    // A hop that cannot be read stops the walk: never an address left of it.
    assert_eq!(
        h.client_of("127.0.0.1", &[("x-forwarded-for", "203.0.113.9, not-an-ip")]),
        Some(ip("127.0.0.1"))
    );
    assert_eq!(
        h.client_of("127.0.0.1", &[("x-forwarded-for", "203.0.113.9, , 192.0.2.10")]),
        Some(ip("127.0.0.1"))
    );
    // The IPv4-mapped form of an address is that address.
    assert_eq!(
        h.client_of("127.0.0.1", &[("x-forwarded-for", "::ffff:203.0.113.9")]),
        Some(ip("203.0.113.9"))
    );

    // Through the router: the chosen hop is the bucket.
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &hops).await, StatusCode::OK);
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &[("x-forwarded-for", "203.0.113.9")])
            .await,
        StatusCode::TOO_MANY_REQUESTS,
        "the rightmost untrusted hop was the client"
    );
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &[("x-forwarded-for", "198.51.100.77")])
            .await,
        StatusCode::OK,
        "the leftmost, client-supplied entry was not the client"
    );
}

// ============================================================================
// Per key, global
// ============================================================================

#[tokio::test]
async fn per_key_buckets_are_separate_and_keys_are_never_logged() {
    const KEY_A: &str = "ares_dummy_limiter_key_alpha_0001";
    const KEY_B: &str = "ares_dummy_limiter_key_bravo_0002";
    const KEY_UNKNOWN: &str = "ares_dummy_limiter_key_unknown_0003";

    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());

    let h = Harness::new(RateLimitConfig {
        per_key_requests_per_minute: 60,
        burst: 2,
        ..off()
    });
    let a = format!("Bearer {KEY_A}");
    let b = format!("Bearer {KEY_B}");
    let unknown = format!("Bearer {KEY_UNKNOWN}");
    let a_hdr = [("authorization", a.as_str())];
    let b_hdr = [("authorization", b.as_str())];

    // Two keys from one client: separate buckets.
    assert_eq!(h.get("/v1/ping", "203.0.113.7", &a_hdr).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "203.0.113.7", &a_hdr).await, StatusCode::OK);
    let refused = h.call(Method::GET, "/v1/ping", "203.0.113.7", &a_hdr).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(&refused), "1");
    assert_eq!(h.get("/v1/ping", "203.0.113.7", &b_hdr).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "203.0.113.7", &b_hdr).await, StatusCode::OK);
    assert_eq!(
        h.get("/v1/ping", "203.0.113.7", &b_hdr).await,
        StatusCode::TOO_MANY_REQUESTS
    );

    // No key: the per-key limit does not apply.
    for _ in 0..5 {
        assert_eq!(h.get("/v1/ping", "203.0.113.7", &[]).await, StatusCode::OK);
    }
    // An unknown key is not authenticated here: it gets its own bucket.
    let unknown_hdr = [("authorization", unknown.as_str())];
    assert_eq!(h.get("/v1/ping", "203.0.113.7", &unknown_hdr).await, StatusCode::OK);
    // The same key from another client shares its bucket.
    assert_eq!(
        h.get("/v1/ping", "198.51.100.9", &a_hdr).await,
        StatusCode::TOO_MANY_REQUESTS
    );

    assert_eq!(
        h.refusals(),
        RefusalCounts {
            global: 0,
            client: 0,
            key: 3
        }
    );

    let warns = capture.warn_lines();
    assert_eq!(warns.len(), 3, "one warn line per refusal: {warns:#?}");
    for line in &warns {
        assert!(line.contains("key"), "the line names the dimension: {line}");
    }
    let text = capture.all_text();
    assert!(!text.is_empty(), "the capture saw the refusals");
    for secret in [
        KEY_A,
        KEY_B,
        KEY_UNKNOWN,
        "limiter_key_alpha",
        "limiter_key_bravo",
        "Bearer",
    ] {
        assert!(!text.contains(secret), "a key leaked into the logs: {secret}\n{text}");
    }
}

#[tokio::test]
async fn global_cap_applies_across_clients() {
    let h = Harness::new(RateLimitConfig {
        global_requests_per_minute: 60,
        burst: 3,
        ..off()
    });
    assert_eq!(h.get("/v1/ping", "192.0.2.1", &[]).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "192.0.2.2", &[]).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "192.0.2.3", &[]).await, StatusCode::OK);
    assert_eq!(
        h.get("/v1/ping", "192.0.2.4", &[]).await,
        StatusCode::TOO_MANY_REQUESTS,
        "a fresh client is refused once the shared cap is spent"
    );
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &[("x-forwarded-for", "203.0.113.7")])
            .await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        h.refusals(),
        RefusalCounts {
            global: 2,
            client: 0,
            key: 0
        }
    );
    h.clock.advance(Duration::from_secs(1));
    assert_eq!(h.get("/v1/ping", "192.0.2.5", &[]).await, StatusCode::OK);
}

// ============================================================================
// Health, the refusal
// ============================================================================

#[tokio::test]
async fn only_exact_get_and_head_health_is_exempt() {
    assert_eq!(EXEMPT_HEALTH_PATHS, &["/health"]);

    let h = Harness::new(RateLimitConfig {
        global_requests_per_minute: 60,
        per_client_requests_per_minute: 60,
        burst: 2,
        ..off()
    });

    // The router really serves the exempt path, and it is never limited.
    for _ in 0..50 {
        assert_eq!(h.get("/health", "203.0.113.7", &[]).await, StatusCode::OK);
        assert_eq!(
            h.call(Method::HEAD, "/health", "203.0.113.7", &[]).await.status(),
            StatusCode::OK
        );
    }
    // Those 100 requests spent nothing: the burst of 2 is intact.
    assert_eq!(h.get("/healthz", "203.0.113.7", &[]).await, StatusCode::NOT_FOUND);
    assert_eq!(h.get("/health/x", "203.0.113.7", &[]).await, StatusCode::NOT_FOUND);

    // Spent. No prefix or look-alike is exempt.
    for path in ["/health/x", "/healthz", "/health/", "/health/context", "/HEALTH"] {
        assert_eq!(
            h.get(path, "203.0.113.7", &[]).await,
            StatusCode::TOO_MANY_REQUESTS,
            "{path} must be limited"
        );
    }
    for method in [Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS] {
        assert_eq!(
            h.call(method.clone(), "/health", "203.0.113.7", &[]).await.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "{method} /health must be limited"
        );
    }
    // The exact GET and HEAD still pass (a query string is not part of the path).
    assert_eq!(h.get("/health", "203.0.113.7", &[]).await, StatusCode::OK);
    assert_eq!(h.get("/health?probe=1", "203.0.113.7", &[]).await, StatusCode::OK);
    assert_eq!(
        h.call(Method::HEAD, "/health", "203.0.113.7", &[]).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn refusal_is_429_json_with_retry_after_in_whole_seconds() {
    let h = Harness::new(RateLimitConfig {
        per_client_requests_per_minute: 1,
        burst: 1,
        ..off()
    });
    assert_eq!(h.get("/v1/ping", "203.0.113.7", &[]).await, StatusCode::OK);

    let refused = h.call(Method::GET, "/v1/ping", "203.0.113.7", &[]).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(&refused), "60", "1 request per minute: next one in 60 s");
    assert_eq!(
        refused
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let body = axum::body::to_bytes(refused.into_body(), 4096)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        json,
        serde_json::json!({"error": "rate limited", "code": "RATE_LIMITED"})
    );

    // Whole seconds, rounded up: 29.5 s left is "30".
    h.clock.advance(Duration::from_millis(30_500));
    let refused = h.call(Method::GET, "/v1/ping", "203.0.113.7", &[]).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(&refused), "30");

    h.clock.advance(Duration::from_millis(29_000));
    let refused = h.call(Method::GET, "/v1/ping", "203.0.113.7", &[]).await;
    assert_eq!(retry_after(&refused), "1", "0.5 s left rounds up to 1, never 0");

    h.clock.advance(Duration::from_millis(500));
    assert_eq!(h.get("/v1/ping", "203.0.113.7", &[]).await, StatusCode::OK);
}

// ============================================================================
// The retired keys
// ============================================================================

#[tokio::test]
async fn old_keys_warn_once_and_limit_nothing() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());

    let mut server = ServerConfig::default();
    server.rate_limit_per_second = 50;
    server.rate_limit_burst = 200;

    let message = legacy_keys_warning(&server).expect("old keys above 0 are warned about");
    for needle in [
        "rate_limit_per_second = 50",
        "rate_limit_burst = 200",
        "one request every 50 s",
        "ignored",
        "[server.rate_limit]",
    ] {
        assert!(message.contains(needle), "missing {needle:?} in: {message}");
    }

    assert!(warn_legacy_keys(&server), "a warning was logged");
    let warns = capture.warn_lines();
    assert_eq!(warns.len(), 1, "exactly one startup warning: {warns:#?}");
    assert!(warns[0].contains("one request every 50 s"), "{}", warns[0]);

    // No limiting from them: the layer main.rs builds from this config admits
    // more than the old burst of 200 from one client, instantly.
    let layer = RateLimitLayer::from_server_config(&server);
    assert!(!layer.is_enabled());
    let h = Harness::with_layer(layer, Arc::new(ManualClock::new()));
    for n in 0..300 {
        assert_eq!(
            h.get("/v1/ping", "127.0.0.1", &[]).await,
            StatusCode::OK,
            "request {n} refused by a retired key"
        );
    }
    assert_eq!(h.refusals(), RefusalCounts::default());

    // Both at 0: silent.
    server.rate_limit_per_second = 0;
    server.rate_limit_burst = 0;
    assert!(legacy_keys_warning(&server).is_none());
    assert!(!warn_legacy_keys(&server));
    assert_eq!(capture.warn_lines().len(), 1, "no second warning");

    // A burst with the old limiter off had no effect, and says so.
    server.rate_limit_burst = 10;
    let message = legacy_keys_warning(&server).expect("burst above 0 is warned about");
    assert!(message.contains("had no effect"), "{message}");

    // The built-in defaults, used when the keys are absent, are 100 and 10.
    let message = legacy_keys_warning(&ServerConfig::default()).expect("defaults are above 0");
    assert!(message.contains("one request every 100 s"), "{message}");
}
