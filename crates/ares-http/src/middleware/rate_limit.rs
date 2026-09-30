//! Proxy-aware, per-client and per-key rate limiting for ARES's HTTP server
//! (CR-3, round A), configured by `[server.rate_limit]` ([`RateLimitConfig`]).
//! **Every limit defaults to 0, which is off**, so the limiter does nothing
//! until a value is set.
//!
//! # Units
//!
//! Each limit is a token bucket in **requests per minute** that holds up to
//! `burst` requests (at least 1). With `per_client_requests_per_minute = 60`
//! and `burst = 5`, a client may send 5 requests at once, then one per
//! second. (The retired `rate_limit_per_second` key was tower_governor's
//! `per_second(n)`, which means one request every `n` seconds; it is ignored,
//! see [`legacy_keys_warning`].)
//!
//! # Dimensions
//!
//! A request is admitted only if every limit that applies to it admits it:
//! - **global**: one bucket shared by every request (a safety cap);
//! - **client**: one bucket per client IP (see *The client*);
//! - **key**: one bucket per `Authorization: Bearer` credential, read with the
//!   API-key middleware's own parser. The bucket is keyed by the SHA-256 of
//!   the credential; the raw value is never stored or logged. Nothing is
//!   authenticated here: an unknown key gets its own bucket and fails
//!   authentication later.
//!
//! A refused request spends nothing from any bucket.
//!
//! # The client
//!
//! The client is the socket peer (`ConnectInfo<SocketAddr>`), unless the peer
//! is in `trusted_proxies`. From a trusted peer, the client is the
//! **rightmost** `X-Forwarded-For` address that is not itself a trusted
//! proxy, or `X-Real-IP` when there is no `X-Forwarded-For`. **A forwarding
//! header from an untrusted peer is never read.** If the walk meets an entry
//! it cannot parse, or finds only trusted proxies, the client is the peer:
//! never an address to the left of a hop that cannot be read. IPv4-mapped
//! IPv6 addresses count as their IPv4 form. A request with no
//! `ConnectInfo` (a server not built with
//! `into_make_service_with_connect_info`) has no known peer: its headers are
//! not read and all such requests share one client bucket.
//!
//! # Exempt
//!
//! Only `GET` and `HEAD` of the exact paths in [`EXEMPT_HEALTH_PATHS`] (the
//! container health check). No prefix match: `/health/x` and `/healthz` are
//! limited.
//!
//! # Refusal
//!
//! `429` with `{"error":"rate limited","code":"RATE_LIMITED"}` (the crate's
//! error body shape), a `Retry-After` header in whole seconds (rounded up, at
//! least 1), one `warn` line naming the binding dimension and the client's
//! network only (IPv4 /24, IPv6 /48), and a per-dimension counter
//! ([`RateLimiter::refusals`]). When several limits refuse, the binding one is
//! the one with the longest wait (ties: global, then client, then key), and
//! `Retry-After` is that wait.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::future::{ready, Either, Ready};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tower::{Layer, Service};

use super::api_key_auth::{extract_api_key, parse_authorization_header};
use crate::config::{RateLimitConfig, ServerConfig};

/// The only paths that are never limited, and only for `GET` and `HEAD`:
/// the container health check (`Dockerfile` `HEALTHCHECK`,
/// `docker-compose.yml`), served by `crate::cordis_routes`. Exact match.
pub const EXEMPT_HEALTH_PATHS: &[&str] = &["/health"];

/// The `code` of a refusal body.
pub const RATE_LIMITED_CODE: &str = "RATE_LIMITED";

/// The `error` of a refusal body.
pub const RATE_LIMITED_MESSAGE: &str = "rate limited";

const NANOS_PER_SECOND: u64 = 1_000_000_000;
const NANOS_PER_MINUTE: u64 = 60 * NANOS_PER_SECOND;

/// A bucket map is swept of idle entries once it holds more than this many
/// entries, and after that whenever it doubles.
const MIN_SWEEP_LEN: usize = 1024;

// ============================================================================
// Clock
// ============================================================================

/// The limiter's time source. Tests use [`ManualClock`].
pub trait Clock: Send + Sync + 'static {
    /// Time since this clock's own origin. Must never go backwards.
    fn now(&self) -> Duration;
}

/// The production clock: monotonic, from [`Instant`].
#[derive(Debug)]
pub struct MonotonicClock {
    origin: Instant,
}

impl MonotonicClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MonotonicClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// A clock that moves only when told to, for tests.
#[derive(Debug, Default)]
pub struct ManualClock {
    nanos: AtomicU64,
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Move the clock forward by `by`.
    pub fn advance(&self, by: Duration) {
        let by = u64::try_from(by.as_nanos()).unwrap_or(u64::MAX);
        let _ = self
            .nanos
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |now| {
                Some(now.saturating_add(by))
            });
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_nanos(self.nanos.load(Ordering::SeqCst))
    }
}

// ============================================================================
// Buckets (GCRA: one "theoretical arrival time" per bucket)
// ============================================================================

/// Which limit refused a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    Global,
    Client,
    Key,
}

impl Dimension {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Client => "client",
            Self::Key => "key",
        }
    }
}

/// Refusals so far, per dimension. Each refusal counts once, against its
/// binding dimension.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RefusalCounts {
    pub global: u64,
    pub client: u64,
    pub key: u64,
}

/// One limit: a request every `interval_ns`, with `tolerance_ns` of burst
/// (`interval_ns * (bucket size - 1)`).
#[derive(Debug, Clone, Copy)]
struct Rate {
    interval_ns: u64,
    tolerance_ns: u64,
}

impl Rate {
    /// `None` when `per_minute` is 0 (the limit is off). A bucket holds
    /// `max(burst, 1)` requests.
    fn new(per_minute: u32, burst: u32) -> Option<Self> {
        if per_minute == 0 {
            return None;
        }
        let interval_ns = NANOS_PER_MINUTE / u64::from(per_minute);
        let size = u64::from(burst.max(1));
        Some(Self {
            interval_ns,
            tolerance_ns: interval_ns.saturating_mul(size - 1),
        })
    }

    /// `Ok(new arrival time)` to admit at `now`, or `Err(wait)` to refuse.
    /// A missing or past arrival time is a full bucket.
    fn check(self, now: u64, tat: Option<u64>) -> Result<u64, u64> {
        let tat = tat.map_or(now, |t| t.max(now));
        let allowed_at = tat.saturating_sub(self.tolerance_ns);
        if now < allowed_at {
            Err(allowed_at - now)
        } else {
            Ok(tat.saturating_add(self.interval_ns))
        }
    }
}

/// Arrival times per bucket key. An entry whose time has passed is a full
/// bucket, the same as no entry, so sweeping it changes nothing.
struct Buckets<K> {
    tats: HashMap<K, u64>,
    sweep_above: usize,
}

impl<K: Hash + Eq> Buckets<K> {
    fn new() -> Self {
        Self {
            tats: HashMap::new(),
            sweep_above: MIN_SWEEP_LEN,
        }
    }

    fn get(&self, key: &K) -> Option<u64> {
        self.tats.get(key).copied()
    }

    fn set(&mut self, key: K, tat: u64, now: u64) {
        self.tats.insert(key, tat);
        if self.tats.len() > self.sweep_above {
            self.tats.retain(|_, t| *t > now);
            self.sweep_above = self.tats.len().saturating_mul(2).max(MIN_SWEEP_LEN);
        }
    }
}

/// `None` is a request with no known peer.
type ClientId = Option<IpAddr>;

struct State {
    global: Option<u64>,
    clients: Buckets<ClientId>,
    keys: Buckets<[u8; 32]>,
}

// ============================================================================
// The limiter
// ============================================================================

/// The shared limiter behind every [`RateLimitService`] of one layer.
pub struct RateLimiter {
    global: Option<Rate>,
    client: Option<Rate>,
    key: Option<Rate>,
    trusted_proxies: HashSet<IpAddr>,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    refused_global: AtomicU64,
    refused_client: AtomicU64,
    refused_key: AtomicU64,
}

struct Refusal {
    dimension: Dimension,
    wait_ns: u64,
    client: ClientId,
}

impl RateLimiter {
    pub fn new(config: &RateLimitConfig) -> Self {
        Self::with_clock(config, Arc::new(MonotonicClock::new()))
    }

    pub fn with_clock(config: &RateLimitConfig, clock: Arc<dyn Clock>) -> Self {
        Self {
            global: Rate::new(config.global_requests_per_minute, config.burst),
            client: Rate::new(config.per_client_requests_per_minute, config.burst),
            key: Rate::new(config.per_key_requests_per_minute, config.burst),
            trusted_proxies: config
                .trusted_proxies
                .iter()
                .map(|ip| ip.to_canonical())
                .collect(),
            clock,
            state: Mutex::new(State {
                global: None,
                clients: Buckets::new(),
                keys: Buckets::new(),
            }),
            refused_global: AtomicU64::new(0),
            refused_client: AtomicU64::new(0),
            refused_key: AtomicU64::new(0),
        }
    }

    /// True when at least one limit is above 0.
    pub fn is_enabled(&self) -> bool {
        self.global.is_some() || self.client.is_some() || self.key.is_some()
    }

    /// Refusals so far, per dimension.
    pub fn refusals(&self) -> RefusalCounts {
        RefusalCounts {
            global: self.refused_global.load(Ordering::Relaxed),
            client: self.refused_client.load(Ordering::Relaxed),
            key: self.refused_key.load(Ordering::Relaxed),
        }
    }

    /// The client a request from socket peer `peer` is limited as (see the
    /// module docs). `None` only when the peer is unknown.
    pub fn client_ip(&self, peer: Option<IpAddr>, headers: &HeaderMap) -> Option<IpAddr> {
        let peer = peer?.to_canonical();
        if !self.trusted_proxies.contains(&peer) {
            return Some(peer);
        }
        let forwarded: Vec<&HeaderValue> = headers.get_all("x-forwarded-for").iter().collect();
        if !forwarded.is_empty() {
            return Some(self.rightmost_untrusted(&forwarded).unwrap_or(peer));
        }
        let real = headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_ip);
        Some(real.unwrap_or(peer))
    }

    /// Walk the `X-Forwarded-For` entries right to left, across header lines
    /// in order, skipping trusted proxies. Stop at the first entry that cannot
    /// be read: nothing to its left can be trusted.
    fn rightmost_untrusted(&self, values: &[&HeaderValue]) -> Option<IpAddr> {
        for value in values.iter().rev() {
            let text = value.to_str().ok()?;
            for entry in text.rsplit(',') {
                let ip = parse_ip(entry)?;
                if !self.trusted_proxies.contains(&ip) {
                    return Some(ip);
                }
            }
        }
        None
    }

    fn counter(&self, dimension: Dimension) -> &AtomicU64 {
        match dimension {
            Dimension::Global => &self.refused_global,
            Dimension::Client => &self.refused_client,
            Dimension::Key => &self.refused_key,
        }
    }

    /// `None` admits the request (and spends from its buckets); `Some` refuses
    /// it (and spends nothing).
    fn decide(&self, req: &Request) -> Option<Refusal> {
        if !self.is_enabled() || is_exempt(req.method(), req.uri().path()) {
            return None;
        }
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| addr.ip());
        let client = self.client_ip(peer, req.headers());
        let key = if self.key.is_some() {
            key_digest(req.headers())
        } else {
            None
        };
        let now = u64::try_from(self.clock.now().as_nanos()).unwrap_or(u64::MAX);

        let mut state = self.state.lock();
        let global = self.global.map(|rate| rate.check(now, state.global));
        let per_client = self
            .client
            .map(|rate| rate.check(now, state.clients.get(&client)));
        let per_key = match (self.key, key.as_ref()) {
            (Some(rate), Some(digest)) => Some(rate.check(now, state.keys.get(digest))),
            _ => None,
        };

        let mut binding: Option<(Dimension, u64)> = None;
        for (dimension, outcome) in [
            (Dimension::Global, global),
            (Dimension::Client, per_client),
            (Dimension::Key, per_key),
        ] {
            if let Some(Err(wait_ns)) = outcome {
                if binding.is_none_or(|(_, longest)| wait_ns > longest) {
                    binding = Some((dimension, wait_ns));
                }
            }
        }
        if let Some((dimension, wait_ns)) = binding {
            drop(state);
            self.counter(dimension).fetch_add(1, Ordering::Relaxed);
            return Some(Refusal {
                dimension,
                wait_ns,
                client,
            });
        }

        if let Some(Ok(tat)) = global {
            state.global = Some(tat);
        }
        if let Some(Ok(tat)) = per_client {
            state.clients.set(client, tat, now);
        }
        if let (Some(Ok(tat)), Some(digest)) = (per_key, key) {
            state.keys.set(digest, tat, now);
        }
        None
    }
}

fn is_exempt(method: &Method, path: &str) -> bool {
    (method == Method::GET || method == Method::HEAD) && EXEMPT_HEALTH_PATHS.contains(&path)
}

/// Parse one forwarding entry: an IP, or an IP with a port. IPv4-mapped IPv6
/// becomes IPv4.
fn parse_ip(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim();
    entry
        .parse::<IpAddr>()
        .ok()
        .or_else(|| entry.parse::<SocketAddr>().ok().map(|addr| addr.ip()))
        .map(|ip| ip.to_canonical())
}

/// SHA-256 of the `Authorization: Bearer` credential, read exactly as the
/// API-key middleware reads it. The raw credential goes no further.
fn key_digest(headers: &HeaderMap) -> Option<[u8; 32]> {
    let auth = parse_authorization_header(headers.get(header::AUTHORIZATION)).ok()?;
    let credential = extract_api_key(auth).ok()?;
    if credential.is_empty() {
        return None;
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&Sha256::digest(credential.as_bytes()));
    Some(digest)
}

/// Whole seconds, rounded up, at least 1.
fn retry_after_secs(wait_ns: u64) -> u64 {
    wait_ns.div_ceil(NANOS_PER_SECOND).max(1)
}

/// The client's network for logs: IPv4 /24, IPv6 /48. Never a full address.
fn client_network(client: ClientId) -> String {
    match client {
        Some(IpAddr::V4(v4)) => {
            let [a, b, c, _] = v4.octets();
            format!("{a}.{b}.{c}.0/24")
        }
        Some(IpAddr::V6(v6)) => {
            let s = v6.segments();
            format!("{}/48", Ipv6Addr::new(s[0], s[1], s[2], 0, 0, 0, 0, 0))
        }
        None => "unknown".to_string(),
    }
}

fn refusal_response(refusal: &Refusal) -> Response {
    let secs = retry_after_secs(refusal.wait_ns);
    tracing::warn!(
        dimension = refusal.dimension.as_str(),
        client_net = %client_network(refusal.client),
        retry_after_secs = secs,
        "request refused by the rate limiter"
    );
    let body = serde_json::json!({
        "error": RATE_LIMITED_MESSAGE,
        "code": RATE_LIMITED_CODE,
    });
    let mut response = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(secs));
    response
}

// ============================================================================
// Tower layer
// ============================================================================

/// The limiter as a tower layer: `router.layer(RateLimitLayer::from_server_config(&server))`.
/// Clones share one [`RateLimiter`].
#[derive(Clone)]
pub struct RateLimitLayer {
    limiter: Arc<RateLimiter>,
}

impl RateLimitLayer {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            limiter: Arc::new(RateLimiter::new(&config)),
        }
    }

    pub fn with_clock(config: RateLimitConfig, clock: Arc<dyn Clock>) -> Self {
        Self {
            limiter: Arc::new(RateLimiter::with_clock(&config, clock)),
        }
    }

    /// The layer for a server: `[server.rate_limit]` only. The retired
    /// `rate_limit_per_second` / `rate_limit_burst` keys are not read.
    pub fn from_server_config(server: &ServerConfig) -> Self {
        Self::new(server.rate_limit.clone())
    }

    pub fn limiter(&self) -> &Arc<RateLimiter> {
        &self.limiter
    }

    pub fn is_enabled(&self) -> bool {
        self.limiter.is_enabled()
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            inner,
            limiter: Arc::clone(&self.limiter),
        }
    }
}

/// The service [`RateLimitLayer`] wraps around each route.
#[derive(Clone)]
pub struct RateLimitService<S> {
    inner: S,
    limiter: Arc<RateLimiter>,
}

impl<S> Service<Request> for RateLimitService<S>
where
    S: Service<Request, Response = Response>,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Either<Ready<Result<Response, S::Error>>, S::Future>;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        match self.limiter.decide(&req) {
            None => Either::Right(self.inner.call(req)),
            Some(refusal) => Either::Left(ready(Ok(refusal_response(&refusal)))),
        }
    }
}

// ============================================================================
// The retired tower_governor keys
// ============================================================================

/// The startup warning for the retired `[server] rate_limit_per_second` and
/// `rate_limit_burst` keys, or `None` when both are 0. The values are not
/// translated: their units were misread, and the new limiter is turned on
/// deliberately.
///
/// The keys default to 100 and 10 when absent, so a config without them is
/// warned about too: those defaults are what the old binary applied.
pub fn legacy_keys_warning(server: &ServerConfig) -> Option<String> {
    let per_second = server.rate_limit_per_second;
    let burst = server.rate_limit_burst;
    if per_second == 0 && burst == 0 {
        return None;
    }
    let meant = if per_second > 0 {
        format!(
            "under tower_governor they meant a burst of {burst}, then one request every \
             {per_second} s per peer IP (not {per_second} per second), and behind a same-host \
             proxy every client shared that one bucket"
        )
    } else {
        "rate_limit_burst had no effect, because rate_limit_per_second = 0 turned the old \
         limiter off"
            .to_string()
    };
    Some(format!(
        "[server] rate_limit_per_second = {per_second} and rate_limit_burst = {burst} are \
         ignored (from the config file, or the built-in defaults 100 and 10 when absent): \
         {meant}. The limiter is now [server.rate_limit], in requests per minute, with every \
         limit 0 (off) until set; the old values are not translated."
    ))
}

/// Log [`legacy_keys_warning`] once, at `warn`. Returns whether it warned.
pub fn warn_legacy_keys(server: &ServerConfig) -> bool {
    match legacy_keys_warning(server) {
        Some(message) => {
            tracing::warn!("{message}");
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_is_off_at_zero_and_holds_at_least_one() {
        assert!(Rate::new(0, 10).is_none());
        let rate = Rate::new(60, 0).expect("on");
        assert_eq!(rate.interval_ns, NANOS_PER_SECOND);
        assert_eq!(rate.tolerance_ns, 0, "burst 0 is a bucket of 1");
        let tat = rate.check(0, None).expect("first request");
        assert_eq!(rate.check(0, Some(tat)), Err(NANOS_PER_SECOND));
    }

    #[test]
    fn rate_units_are_per_minute() {
        let rate = Rate::new(120, 1).expect("on");
        assert_eq!(rate.interval_ns, NANOS_PER_SECOND / 2);
        let rate = Rate::new(u32::MAX, u32::MAX).expect("on");
        assert!(rate.interval_ns > 0, "never a zero interval");
    }

    #[test]
    fn retry_after_rounds_up_to_whole_seconds() {
        assert_eq!(retry_after_secs(1), 1);
        assert_eq!(retry_after_secs(NANOS_PER_SECOND), 1);
        assert_eq!(retry_after_secs(NANOS_PER_SECOND + 1), 2);
        assert_eq!(retry_after_secs(0), 1);
    }

    #[test]
    fn logged_client_is_truncated() {
        assert_eq!(
            client_network(Some("203.0.113.7".parse().unwrap())),
            "203.0.113.0/24"
        );
        assert_eq!(client_network(Some("::1".parse().unwrap())), "::/48");
        assert_eq!(client_network(None), "unknown");
    }

    #[test]
    fn idle_buckets_are_swept_without_changing_a_decision() {
        let mut buckets: Buckets<u32> = Buckets::new();
        for n in 0..=(MIN_SWEEP_LEN as u32) {
            buckets.set(n, 5, 0);
        }
        assert_eq!(buckets.tats.len(), MIN_SWEEP_LEN + 1, "all live: none swept");
        buckets.set(u32::MAX, 20, 10);
        assert_eq!(buckets.tats.len(), MIN_SWEEP_LEN + 2, "below the new mark");
        let mut buckets: Buckets<u32> = Buckets::new();
        for n in 0..(MIN_SWEEP_LEN as u32) {
            buckets.set(n, 5, 0);
        }
        buckets.set(u32::MAX, 20, 10);
        assert_eq!(buckets.tats.len(), 1, "only the live entry is kept");
        assert_eq!(buckets.get(&u32::MAX), Some(20));
    }
}
