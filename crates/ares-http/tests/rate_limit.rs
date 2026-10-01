//! The client limiter (CR-3, round A): `ares_http::middleware::rate_limit`,
//! driven through ARES's real router (`ares_http::build_router`).
//!
//! Pure: no database, no network. Time comes from a `ManualClock`, so no
//! test sleeps. Addresses come only from the documentation ranges
//! (192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24, 2001:db8::/32) and
//! loopback. Keys are dummies.

#![cfg(feature = "postgres")]

use std::alloc::GlobalAlloc;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ares_http::config::{RateLimitConfig, ServerConfig};
use ares_http::middleware::rate_limit::{
    legacy_keys_warning, warn_legacy_keys, ManualClock, RateLimitLayer, RefusalCounts, Tracked,
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
// Heap bytes, per thread: a counting global allocator. Each test runs on its
// own thread and the limiter allocates on the thread that calls it, so a test
// reads its own bytes, whatever the other tests do in parallel. (Gate 2:
// `capacity()` is not a byte count; tombstones lower it with nothing freed.)
// ============================================================================

struct CountingAlloc;

thread_local! {
    static HEAP_LIVE: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    static HEAP_PEAK: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

fn heap_count(delta: isize) {
    let _ = HEAP_LIVE.try_with(|live| {
        let now = live.get() + delta;
        live.set(now);
        let _ = HEAP_PEAK.try_with(|peak| {
            if now > peak.get() {
                peak.set(now);
            }
        });
    });
}

unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let ptr = unsafe { std::alloc::System.alloc(layout) };
        if !ptr.is_null() {
            heap_count(layout.size() as isize);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        let ptr = unsafe { std::alloc::System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            heap_count(layout.size() as isize);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) };
        heap_count(-(layout.size() as isize));
    }

    unsafe fn realloc(
        &self,
        ptr: *mut u8,
        layout: std::alloc::Layout,
        new_size: usize,
    ) -> *mut u8 {
        let moved = unsafe { std::alloc::System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            heap_count(new_size as isize - layout.size() as isize);
        }
        moved
    }
}

#[global_allocator]
static HEAP: CountingAlloc = CountingAlloc;

/// Bytes allocated and not yet freed on this thread.
fn heap_live() -> isize {
    HEAP_LIVE.with(|live| live.get())
}

/// The most bytes held on this thread since the last reset.
fn heap_peak() -> isize {
    HEAP_PEAK.with(|peak| peak.get())
}

fn heap_reset_peak() {
    let now = heap_live();
    HEAP_PEAK.with(|peak| peak.set(now));
}

/// The limiter in front of a service that always answers 200, driven
/// synchronously: millions of requests in a test, without the router's cost.
type OkFn = fn(Request<Body>) -> futures::future::Ready<Result<Response, std::convert::Infallible>>;

fn always_ok(_req: Request<Body>) -> futures::future::Ready<Result<Response, std::convert::Infallible>> {
    futures::future::ready(Ok(Response::new(Body::empty())))
}

struct Direct {
    svc: ares_http::middleware::rate_limit::RateLimitService<tower::util::ServiceFn<OkFn>>,
    layer: RateLimitLayer,
    clock: Arc<ManualClock>,
}

impl Direct {
    fn new(config: RateLimitConfig) -> Self {
        let clock = Arc::new(ManualClock::new());
        let layer = RateLimitLayer::with_clock(config, clock.clone());
        let svc = tower::Layer::layer(&layer, tower::service_fn(always_ok as OkFn));
        Self { svc, layer, clock }
    }

    fn send(&mut self, peer: IpAddr, authorization: Option<&str>) -> StatusCode {
        use futures::FutureExt;
        let mut builder = Request::builder().method(Method::GET).uri("/v1/ping");
        if let Some(value) = authorization {
            builder = builder.header("authorization", value);
        }
        let mut req = builder.body(Body::empty()).expect("request");
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(peer, 40_000)));
        tower::Service::call(&mut self.svc, req)
            .now_or_never()
            .expect("the limiter answers synchronously")
            .expect("infallible")
            .status()
    }
}

/// The n-th /64 in 2001:db8::/32.
fn net64(n: u32) -> IpAddr {
    IpAddr::V6(std::net::Ipv6Addr::new(
        0x2001,
        0x0db8,
        (n >> 16) as u16,
        n as u16,
        0,
        0,
        0,
        1,
    ))
}

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
        self.send(method, path, Some(peer), headers).await
    }

    /// `peer = None`: a server that was not built with
    /// `into_make_service_with_connect_info` (no `ConnectInfo` extension).
    async fn send(
        &self,
        method: Method,
        path: &str,
        peer: Option<&str>,
        headers: &[(&str, &str)],
    ) -> Response {
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let mut req = builder.body(Body::empty()).expect("request");
        if let Some(peer) = peer {
            req.extensions_mut()
                .insert(ConnectInfo(SocketAddr::new(ip(peer), 40_000)));
        }
        self.router.clone().oneshot(req).await.expect("infallible")
    }

    fn tracked(&self) -> Tracked {
        self.layer.limiter().tracked()
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
    assert_eq!(config.max_tracked_clients, 100_000);
    assert_eq!(config.max_tracked_keys, 100_000);

    let h = Harness::new(config);
    assert!(
        !h.layer.is_enabled(),
        "every limit at 0 means the limiter is off"
    );

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
            .get(
                "/v1/ping",
                "127.0.0.1",
                &[("x-forwarded-for", "198.51.100.9")],
            )
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "proxied request {n} was refused while off"
        );
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
max_tracked_clients = 5000
max_tracked_keys = 7000
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
    assert_eq!(rl.max_tracked_clients, 5000);
    assert_eq!(rl.max_tracked_keys, 7000);
    assert!(rl.any_limit_set());

    // trusted_proxies takes exact addresses: a CIDR range fails the load.
    let cidr =
        toml::from_str::<ServerConfig>("[rate_limit]\ntrusted_proxies = [\"127.0.0.0/8\"]\n");
    assert!(cidr.is_err(), "a CIDR trusted proxy must not load");

    // No table at all: every limit off.
    let bare: ServerConfig = toml::from_str("host = \"127.0.0.1\"\n").expect("bare server");
    assert_eq!(bare.rate_limit, RateLimitConfig::default());
    assert!(!bare.rate_limit.any_limit_set());

    // A key without its unit is refused at load, never silently ignored.
    let unitless = toml::from_str::<ServerConfig>("[rate_limit]\nper_client = 60\n");
    assert!(
        unitless.is_err(),
        "an unknown [rate_limit] key must not load"
    );

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
    assert_eq!(
        sixth.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the 6th in a burst of 5"
    );
    assert_eq!(
        retry_after(&sixth),
        "1",
        "60 per minute refills one request per second"
    );

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
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &a).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &b).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &b).await, StatusCode::OK);
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &b).await,
        StatusCode::TOO_MANY_REQUESTS
    );

    // The proxy's own requests (no forwarding header) are a third client.
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &[]).await, StatusCode::OK);

    // ::1 and the IPv4-mapped loopback are the same trusted proxy.
    assert_eq!(
        h.get("/v1/ping", "::1", &a).await,
        StatusCode::TOO_MANY_REQUESTS
    );
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
        h.get(
            "/v1/ping",
            "127.0.0.1",
            &[("x-forwarded-for", "198.51.100.20")]
        )
        .await,
        StatusCode::TOO_MANY_REQUESTS,
        "X-Real-IP and X-Forwarded-For naming one address are one client"
    );
    // With both headers, X-Forwarded-For wins.
    assert_eq!(
        h.client_of(
            "127.0.0.1",
            &[
                ("x-forwarded-for", "203.0.113.30"),
                ("x-real-ip", "198.51.100.20")
            ]
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
    assert_eq!(
        via_untrusted,
        Some(ip("198.51.100.9")),
        "the header from an untrusted peer is ignored"
    );

    // The untrusted peer spends its own bucket, whatever it claims.
    assert_eq!(
        h.get("/v1/ping", "198.51.100.9", &spoof).await,
        StatusCode::OK
    );
    assert_eq!(
        h.get("/v1/ping", "198.51.100.9", &spoof).await,
        StatusCode::OK
    );
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
        assert!(
            line.contains("client"),
            "the line names the dimension: {line}"
        );
        assert!(
            line.contains("198.51.100.0/24"),
            "the line carries the /24: {line}"
        );
    }
    let text = capture.all_text();
    for full in ["198.51.100.9", "203.0.113.7", "203.0.113.8", "203.0.113.9"] {
        assert!(
            !text.contains(full),
            "a full address was logged: {full}\n{text}"
        );
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

    let hops = [("x-forwarded-for", "198.51.100.77, 203.0.113.9, 192.0.2.10")];
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
        h.client_of(
            "127.0.0.1",
            &[("x-forwarded-for", "203.0.113.9,192.0.2.10")]
        ),
        Some(ip("203.0.113.9"))
    );
    // Every hop trusted: the peer is the client.
    assert_eq!(
        h.client_of(
            "127.0.0.1",
            &[("x-forwarded-for", "192.0.2.11, 192.0.2.10")]
        ),
        Some(ip("127.0.0.1"))
    );
    // A hop that cannot be read stops the walk: never an address left of it.
    assert_eq!(
        h.client_of(
            "127.0.0.1",
            &[("x-forwarded-for", "203.0.113.9, not-an-ip")]
        ),
        Some(ip("127.0.0.1"))
    );
    assert_eq!(
        h.client_of(
            "127.0.0.1",
            &[("x-forwarded-for", "203.0.113.9, , 192.0.2.10")]
        ),
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
        h.get(
            "/v1/ping",
            "127.0.0.1",
            &[("x-forwarded-for", "203.0.113.9")]
        )
        .await,
        StatusCode::TOO_MANY_REQUESTS,
        "the rightmost untrusted hop was the client"
    );
    assert_eq!(
        h.get(
            "/v1/ping",
            "127.0.0.1",
            &[("x-forwarded-for", "198.51.100.77")]
        )
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
    assert_eq!(
        h.get("/v1/ping", "203.0.113.7", &a_hdr).await,
        StatusCode::OK
    );
    assert_eq!(
        h.get("/v1/ping", "203.0.113.7", &a_hdr).await,
        StatusCode::OK
    );
    let refused = h.call(Method::GET, "/v1/ping", "203.0.113.7", &a_hdr).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(&refused), "1");
    assert_eq!(
        h.get("/v1/ping", "203.0.113.7", &b_hdr).await,
        StatusCode::OK
    );
    assert_eq!(
        h.get("/v1/ping", "203.0.113.7", &b_hdr).await,
        StatusCode::OK
    );
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
    assert_eq!(
        h.get("/v1/ping", "203.0.113.7", &unknown_hdr).await,
        StatusCode::OK
    );
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
        assert!(
            !text.contains(secret),
            "a key leaked into the logs: {secret}\n{text}"
        );
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
        h.get(
            "/v1/ping",
            "127.0.0.1",
            &[("x-forwarded-for", "203.0.113.7")]
        )
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
            h.call(Method::HEAD, "/health", "203.0.113.7", &[])
                .await
                .status(),
            StatusCode::OK
        );
    }
    // Those 100 requests spent nothing: the burst of 2 is intact.
    assert_eq!(
        h.get("/healthz", "203.0.113.7", &[]).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.get("/health/x", "203.0.113.7", &[]).await,
        StatusCode::NOT_FOUND
    );

    // Spent. No prefix or look-alike is exempt.
    for path in [
        "/health/x",
        "/healthz",
        "/health/",
        "/health/context",
        "/HEALTH",
    ] {
        assert_eq!(
            h.get(path, "203.0.113.7", &[]).await,
            StatusCode::TOO_MANY_REQUESTS,
            "{path} must be limited"
        );
    }
    for method in [Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS] {
        assert_eq!(
            h.call(method.clone(), "/health", "203.0.113.7", &[])
                .await
                .status(),
            StatusCode::TOO_MANY_REQUESTS,
            "{method} /health must be limited"
        );
    }
    // The exact GET and HEAD still pass (a query string is not part of the path).
    assert_eq!(h.get("/health", "203.0.113.7", &[]).await, StatusCode::OK);
    assert_eq!(
        h.get("/health?probe=1", "203.0.113.7", &[]).await,
        StatusCode::OK
    );
    assert_eq!(
        h.call(Method::HEAD, "/health", "203.0.113.7", &[])
            .await
            .status(),
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
    assert_eq!(
        retry_after(&refused),
        "60",
        "1 request per minute: next one in 60 s"
    );
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
    assert_eq!(
        retry_after(&refused),
        "1",
        "0.5 s left rounds up to 1, never 0"
    );

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

    let mut server = ServerConfig {
        rate_limit_per_second: 50,
        rate_limit_burst: 200,
        ..ServerConfig::default()
    };

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

    // rate_limit_burst = 0 with the old limiter on: tower_governor's finish()
    // returned None, so the old binary panicked at boot. Say so, not "a burst of 0".
    server.rate_limit_per_second = 50;
    server.rate_limit_burst = 0;
    let message = legacy_keys_warning(&server).expect("per_second above 0 is warned about");
    assert!(message.contains("panicked at boot"), "{message}");
    assert!(!message.contains("a burst of 0"), "{message}");
}

// ============================================================================
// Fix round 1
// ============================================================================

/// rev1 F1 / rev2 F3: an IPv6 client is its /64, in the bucket and in the log.
#[tokio::test]
async fn ipv6_clients_share_a_bucket_per_64() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());

    let h = Harness::new(RateLimitConfig {
        per_client_requests_per_minute: 60,
        burst: 1,
        ..off()
    });

    // Direct peers: one /64 is one client; the next /64 is another.
    assert_eq!(
        h.get("/v1/ping", "2001:db8:1:2::1", &[]).await,
        StatusCode::OK
    );
    assert_eq!(
        h.get("/v1/ping", "2001:db8:1:2:ffff:ffff:ffff:fffe", &[])
            .await,
        StatusCode::TOO_MANY_REQUESTS,
        "another address in the same /64 shares the bucket"
    );
    assert_eq!(
        h.get("/v1/ping", "2001:db8:1:3::1", &[]).await,
        StatusCode::OK,
        "a different /64 is a different client"
    );

    // Behind the trusted proxy, the same.
    let a = [("x-forwarded-for", "2001:db8:5:6::7")];
    let same_64 = [("x-forwarded-for", "2001:db8:5:6:a:b:c:d")];
    let other_64 = [("x-forwarded-for", "2001:db8:5:7::7")];
    assert_eq!(h.get("/v1/ping", "127.0.0.1", &a).await, StatusCode::OK);
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &same_64).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &other_64).await,
        StatusCode::OK
    );

    // IPv4-mapped IPv6 is IPv4: a bucket per address, not per /64.
    assert_eq!(
        h.get("/v1/ping", "::ffff:203.0.113.7", &[]).await,
        StatusCode::OK
    );
    assert_eq!(
        h.get("/v1/ping", "203.0.113.7", &[]).await,
        StatusCode::TOO_MANY_REQUESTS,
        "the mapped and plain forms are one client"
    );
    assert_eq!(
        h.get("/v1/ping", "::ffff:203.0.113.8", &[]).await,
        StatusCode::OK,
        "two mapped IPv4 addresses are two clients, not one /64"
    );

    // The log names the /64, never an address.
    let warns = capture.warn_lines();
    assert_eq!(warns.len(), 3, "one line per refusal: {warns:#?}");
    assert!(
        warns[0].contains("client_net=2001:db8:1:2::/64"),
        "{}",
        warns[0]
    );
    assert!(
        warns[1].contains("client_net=2001:db8:5:6::/64"),
        "{}",
        warns[1]
    );
    assert!(
        warns[2].contains("client_net=203.0.113.0/24"),
        "{}",
        warns[2]
    );
    let text = capture.all_text();
    for full in [
        "2001:db8:1:2::1",
        "2001:db8:1:2:ffff",
        "2001:db8:5:6::7",
        "2001:db8:5:6:a",
        "203.0.113.7",
    ] {
        assert!(
            !text.contains(full),
            "a full address was logged: {full}\n{text}"
        );
    }
}

/// rev1 gate-2 F1: X-Real-IP folded into one comma-joined line (RFC 9110
/// §5.3 lets an intermediary fold the two-line probe below) is not one
/// address: the client is the peer, and the leftmost entry's bucket is never
/// spent. Red under rev1's leftmost-entry mutant.
#[tokio::test]
async fn x_real_ip_comma_joined_in_one_line_is_not_read() {
    let h = Harness::new(RateLimitConfig {
        per_client_requests_per_minute: 60,
        burst: 1,
        ..off()
    });
    for folded in [
        "203.0.113.66, 198.51.100.20",
        "203.0.113.66,198.51.100.20",
        "203.0.113.66,",
    ] {
        assert_eq!(
            h.client_of("127.0.0.1", &[("x-real-ip", folded)]),
            Some(ip("127.0.0.1")),
            "X-Real-IP: {folded}"
        );
    }
    let folded = [("x-real-ip", "203.0.113.66, 198.51.100.20")];
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &folded).await,
        StatusCode::OK,
        "limited as the peer"
    );
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &folded).await,
        StatusCode::TOO_MANY_REQUESTS,
        "the peer's bucket is spent"
    );
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &[("x-real-ip", "203.0.113.66")])
            .await,
        StatusCode::OK,
        "203.0.113.66's bucket was never spent"
    );
}

/// rev1 F2: X-Real-IP is read only when it has exactly one line. rev1's
/// probe sent the client's line first and the proxy's second.
#[tokio::test]
async fn x_real_ip_with_more_than_one_line_is_not_read() {
    let h = Harness::new(RateLimitConfig {
        per_client_requests_per_minute: 60,
        burst: 2,
        ..off()
    });
    let two_lines = [
        ("x-real-ip", "198.51.100.20"),
        ("x-real-ip", "203.0.113.66"),
    ];
    let victim = [("x-real-ip", "198.51.100.20")];

    assert_eq!(
        h.client_of("127.0.0.1", &two_lines),
        Some(ip("127.0.0.1")),
        "more than one line: the client is the peer"
    );
    assert_eq!(
        h.client_of(
            "127.0.0.1",
            &[
                ("x-real-ip", "203.0.113.66"),
                ("x-real-ip", "198.51.100.20")
            ]
        ),
        Some(ip("127.0.0.1"))
    );
    assert_eq!(
        h.client_of("127.0.0.1", &victim),
        Some(ip("198.51.100.20")),
        "exactly one line is read"
    );

    // The two-line request spends the proxy's bucket, never the victim's.
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &two_lines).await,
        StatusCode::OK
    );
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &two_lines).await,
        StatusCode::OK
    );
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &two_lines).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &victim).await,
        StatusCode::OK,
        "the client could not choose the victim's bucket"
    );
    assert_eq!(
        h.get("/v1/ping", "127.0.0.1", &victim).await,
        StatusCode::OK
    );
}

/// rev2 F1: interpretation 5 (a refused request spends nothing) and the
/// longest-wait rule, with two dimensions at two rates. rev2's S1 (a refused
/// request spends the admitting buckets) and S2 (the first refuser binds)
/// each turn this red.
#[tokio::test]
async fn a_refusal_spends_nothing_and_the_longest_wait_binds() {
    const KEY: &str = "Bearer ares_dummy_limiter_key_two_rates_0004";
    let key = [("authorization", KEY)];

    // Client 60/min (a token every 1 s), key 1/min (every 60 s), bucket of 1.
    let h = Harness::new(RateLimitConfig {
        per_client_requests_per_minute: 60,
        per_key_requests_per_minute: 1,
        burst: 1,
        ..off()
    });
    assert_eq!(h.get("/v1/ping", "203.0.113.7", &key).await, StatusCode::OK);

    // 1 s later the client has a token again; the key's is 59 s away.
    h.clock.advance(Duration::from_secs(1));
    let refused = h.call(Method::GET, "/v1/ping", "203.0.113.7", &key).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(&refused), "59", "the key's wait");
    assert_eq!(
        h.refusals(),
        RefusalCounts {
            global: 0,
            client: 0,
            key: 1
        }
    );
    // That refusal spent nothing: the client's token is still there.
    assert_eq!(
        h.get("/v1/ping", "203.0.113.7", &[]).await,
        StatusCode::OK,
        "a key refusal must not spend the client bucket (S1)"
    );

    // Now both refuse: the client's wait is 1 s, the key's 59 s. The longest
    // binds, for Retry-After and for the counter.
    let refused = h.call(Method::GET, "/v1/ping", "203.0.113.7", &key).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        retry_after(&refused),
        "59",
        "the longest wait binds, not the first refuser (S2)"
    );
    assert_eq!(
        h.refusals(),
        RefusalCounts {
            global: 0,
            client: 0,
            key: 2
        },
        "counted against the binding (longest-wait) dimension"
    );

    // The converse: client 1/min, key 60/min. The client's wait binds, and a
    // client refusal leaves the key bucket untouched.
    let h = Harness::new(RateLimitConfig {
        per_client_requests_per_minute: 1,
        per_key_requests_per_minute: 60,
        burst: 1,
        ..off()
    });
    assert_eq!(
        h.get("/v1/ping", "198.51.100.9", &key).await,
        StatusCode::OK
    );
    h.clock.advance(Duration::from_secs(1));
    let refused = h.call(Method::GET, "/v1/ping", "198.51.100.9", &key).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(&refused), "59", "the client's wait");
    assert_eq!(
        h.refusals(),
        RefusalCounts {
            global: 0,
            client: 1,
            key: 0
        }
    );
    assert_eq!(
        h.get("/v1/ping", "198.51.100.10", &key).await,
        StatusCode::OK,
        "a client refusal must not spend the key bucket (S1)"
    );
}

/// rev2 F2: the client map has a ceiling. Full of live buckets, a new client
/// shares one overflow bucket at the same rate; expired buckets are swept
/// before a new one is added; the overflow is logged once a minute, a count.
#[tokio::test]
async fn client_map_is_capped_and_new_clients_share_an_overflow_bucket() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let overflow_lines = |c: &Capture| -> Vec<String> {
        c.warn_lines()
            .into_iter()
            .filter(|l| l.contains("overflow"))
            .collect()
    };

    let h = Harness::new(RateLimitConfig {
        per_client_requests_per_minute: 60,
        burst: 1,
        max_tracked_clients: 2,
        ..off()
    });
    assert_eq!(h.get("/v1/ping", "192.0.2.1", &[]).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "192.0.2.2", &[]).await, StatusCode::OK);
    assert_eq!(h.tracked().clients, 2);

    // Full of live buckets: the next new client takes the overflow bucket's
    // token, and the one after it is refused at the same rate.
    assert_eq!(h.get("/v1/ping", "192.0.2.3", &[]).await, StatusCode::OK);
    let refused = h.call(Method::GET, "/v1/ping", "192.0.2.4", &[]).await;
    assert_eq!(
        refused.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the overflow bucket is limited, never unlimited"
    );
    assert_eq!(retry_after(&refused), "1");
    assert_eq!(h.tracked().clients, 2, "the cap holds");
    assert_eq!(
        h.get("/v1/ping", "192.0.2.1", &[]).await,
        StatusCode::TOO_MANY_REQUESTS,
        "a tracked client keeps its own bucket"
    );
    assert_eq!(h.refusals().client, 2);

    let lines = overflow_lines(&capture);
    assert_eq!(lines.len(), 1, "logged once: {lines:#?}");
    assert!(lines[0].contains("dimension=\"client\""), "{}", lines[0]);
    assert!(lines[0].contains("requests=1"), "{}", lines[0]);
    assert!(!lines[0].contains("192.0.2."), "no address: {}", lines[0]);

    // 1 s later the tracked buckets are full again: they are swept before a
    // new client is added, and the new clients are tracked.
    h.clock.advance(Duration::from_secs(1));
    assert_eq!(h.get("/v1/ping", "192.0.2.5", &[]).await, StatusCode::OK);
    assert_eq!(h.tracked().clients, 1, "expired buckets swept first");
    assert_eq!(h.get("/v1/ping", "192.0.2.6", &[]).await, StatusCode::OK);
    assert_eq!(h.tracked().clients, 2);
    // Full again: the overflow bucket (its token is back) serves the next one.
    assert_eq!(h.get("/v1/ping", "192.0.2.7", &[]).await, StatusCode::OK);
    assert_eq!(h.tracked().clients, 2);
    assert_eq!(
        overflow_lines(&capture).len(),
        1,
        "within the minute: no new line"
    );

    // A minute on, the next overflow logs the count since the last line.
    h.clock.advance(Duration::from_secs(60));
    assert_eq!(h.get("/v1/ping", "192.0.2.8", &[]).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "192.0.2.9", &[]).await, StatusCode::OK);
    assert_eq!(h.get("/v1/ping", "192.0.2.10", &[]).await, StatusCode::OK);
    assert_eq!(h.tracked().clients, 2);
    let lines = overflow_lines(&capture);
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(lines[1].contains("requests=3"), "{}", lines[1]);
    for line in &lines {
        assert!(!line.contains("192.0.2."), "no address: {line}");
    }
}

/// rev2 F2: the key map has a ceiling too, with the same overflow.
#[tokio::test]
async fn key_map_is_capped_and_new_keys_share_an_overflow_bucket() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());

    let h = Harness::new(RateLimitConfig {
        per_key_requests_per_minute: 60,
        burst: 1,
        max_tracked_keys: 2,
        ..off()
    });
    let k = |n: u32| format!("Bearer ares_dummy_limiter_key_cap_{n:04}");
    for n in 1..=2 {
        let auth = k(n);
        assert_eq!(
            h.get(
                "/v1/ping",
                "203.0.113.7",
                &[("authorization", auth.as_str())]
            )
            .await,
            StatusCode::OK
        );
    }
    assert_eq!(h.tracked().keys, 2);
    let k3 = k(3);
    let k4 = k(4);
    let k1 = k(1);
    assert_eq!(
        h.get("/v1/ping", "203.0.113.7", &[("authorization", k3.as_str())])
            .await,
        StatusCode::OK,
        "the overflow bucket's token"
    );
    assert_eq!(
        h.get("/v1/ping", "203.0.113.7", &[("authorization", k4.as_str())])
            .await,
        StatusCode::TOO_MANY_REQUESTS,
        "the overflow bucket is limited"
    );
    assert_eq!(h.tracked().keys, 2, "the cap holds");
    assert_eq!(
        h.get("/v1/ping", "203.0.113.7", &[("authorization", k1.as_str())])
            .await,
        StatusCode::TOO_MANY_REQUESTS,
        "a tracked key keeps its own bucket"
    );
    assert_eq!(h.refusals().key, 2);

    let lines: Vec<String> = capture
        .warn_lines()
        .into_iter()
        .filter(|l| l.contains("overflow"))
        .collect();
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(lines[0].contains("dimension=\"key\""), "{}", lines[0]);
    let text = capture.all_text();
    for secret in ["ares_dummy_limiter_key_cap", "Bearer"] {
        assert!(!text.contains(secret), "a key leaked: {secret}\n{text}");
    }
}

/// rev2 F2 (gates 1 and 2): a sweep that frees most of the table gives the
/// memory back. Measured in heap bytes on this thread, not `capacity()`,
/// which tombstones lower with nothing freed. Red with the shrink removed.
#[test]
fn memory_returns_after_a_sweep() {
    let mut d = Direct::new(RateLimitConfig {
        per_client_requests_per_minute: 60,
        burst: 1,
        ..off()
    });
    let base = heap_live();
    for n in 0..7000u32 {
        assert_eq!(d.send(net64(n), None), StatusCode::OK);
    }
    let full = heap_live() - base;
    let before = d.layer.limiter().tracked();
    assert_eq!(before.clients, 7000);

    // 2 s on, every one of those buckets is full again (expired). New clients
    // arrive until a sweep runs (the held count drops); the sweep must drop
    // the old entries and free the table's memory.
    d.clock.advance(Duration::from_secs(2));
    let mut swept = None;
    for n in 0..20_000u32 {
        assert_eq!(d.send(net64(1_000_000 + n), None), StatusCode::OK);
        let now = d.layer.limiter().tracked();
        if now.clients < before.clients + n as usize + 1 {
            swept = Some((n as usize, now));
            break;
        }
    }
    let (n, after) = swept.expect("a sweep ran before the table grew");
    let held = heap_live() - base;
    eprintln!("memory_returns_after_a_sweep: {full} B for 7000 buckets -> {held} B for {} after the sweep", after.clients);
    assert!(
        after.clients <= n + 1,
        "only the live buckets remain: {before:?} -> {after:?}"
    );
    assert!(
        held * 4 < full,
        "the sweep freed the table's memory: {full} B -> {held} B"
    );
}

/// rev2 gate-2 F1: under churn at the default caps (100 000), each table
/// stays within its cap's size: 131 072 slots, 3 276 816 bytes for clients
/// (24-byte entries) and 5 373 968 for keys (40-byte entries), the bound in
/// the config docs. During a sweep the live entries are copied out once, so
/// the peak is at most `cap` entries more. 70 000 identities stay live and
/// 40 000 new ones arrive every second. Measured in heap bytes on this thread.
#[test]
fn client_and_key_tables_stay_at_their_caps_size_under_churn() {
    const SLACK: isize = 64 * 1024;
    for keys in [false, true] {
        let label = if keys { "keys" } else { "clients" };
        let entry: isize = if keys { 40 } else { 24 };
        let table_bound = 131_072 * (entry + 1) + 16;
        let peak_bound = table_bound + 100_000 * entry;
        let mut d = Direct::new(RateLimitConfig {
            per_client_requests_per_minute: if keys { 0 } else { 60 },
            per_key_requests_per_minute: if keys { 60 } else { 0 },
            burst: 2,
            ..off()
        });
        let fixed = ip("203.0.113.9");
        let base = heap_live();
        heap_reset_peak();
        let mut worst = 0isize;
        let mut next = 10_000_000u32;
        for tick in 0..10 {
            for i in 0..70_000u32 {
                if keys {
                    let auth = format!("Bearer ares_dummy_limiter_churn_p_{i:08}");
                    d.send(fixed, Some(&auth));
                } else {
                    d.send(net64(i), None);
                }
            }
            for _ in 0..40_000 {
                next += 1;
                if keys {
                    let auth = format!("Bearer ares_dummy_limiter_churn_o_{next:08}");
                    d.send(fixed, Some(&auth));
                } else {
                    d.send(net64(next), None);
                }
            }
            let held = heap_live() - base;
            worst = worst.max(held);
            assert!(
                held <= table_bound + SLACK,
                "{label}, second {tick}: {held} B held; the cap's table is {table_bound} B ({:?})",
                d.layer.limiter().tracked()
            );
            d.clock.advance(Duration::from_secs(1));
        }
        let peak = heap_peak() - base;
        eprintln!(
            "churn {label}: worst held {worst} B (bound {table_bound}), peak {peak} B (bound {peak_bound})"
        );
        assert!(
            peak <= peak_bound + SLACK,
            "{label}: peak {peak} B; the bound is {peak_bound} B"
        );
        let tracked = d.layer.limiter().tracked();
        assert_eq!(
            if keys { tracked.keys } else { tracked.clients },
            100_000,
            "the cap is reached and held"
        );
        drop(d);
        assert!(
            heap_live() - base <= SLACK,
            "{label}: dropping the limiter frees its tables"
        );
    }
}

/// rev1 F3: with no ConnectInfo, one warning per process while enabled;
/// behaviour is unchanged (one shared client bucket). This is the only test
/// in this binary that sends a request with no peer.
#[tokio::test]
async fn no_connect_info_warns_once_per_process() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());

    let h = Harness::new(RateLimitConfig {
        per_client_requests_per_minute: 60,
        burst: 1,
        ..off()
    });
    let first = h.send(Method::GET, "/v1/ping", None, &[]).await;
    assert_eq!(first.status(), StatusCode::OK);
    for _ in 0..3 {
        let next = h.send(Method::GET, "/v1/ping", None, &[]).await;
        assert_eq!(
            next.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "unchanged: requests with no peer share one client bucket"
        );
    }
    let peerless: Vec<String> = capture
        .warn_lines()
        .into_iter()
        .filter(|l| l.contains("ConnectInfo"))
        .collect();
    assert_eq!(peerless.len(), 1, "one warning per process: {peerless:#?}");
    let refusals: Vec<String> = capture
        .warn_lines()
        .into_iter()
        .filter(|l| l.contains("client_net=unknown"))
        .collect();
    assert_eq!(refusals.len(), 3, "{refusals:#?}");
}
