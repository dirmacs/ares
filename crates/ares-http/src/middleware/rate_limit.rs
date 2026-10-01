//! Proxy-aware, per-client and per-key rate limiting for ARES's HTTP server
//! (CR-3, round A), configured by `[server.rate_limit]` ([`RateLimitConfig`]).
//! **Every limit defaults to 0, which is off**, so the limiter does nothing
//! until a value is set. Limits are read at startup: a change needs a restart.
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
//! - **client**: one bucket per client (see *The client*);
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
//! The client address is the socket peer (`ConnectInfo<SocketAddr>`), unless
//! the peer is in `trusted_proxies` (exact addresses). From a trusted peer,
//! it is the **rightmost** `X-Forwarded-For` address that is not itself a
//! trusted proxy, or `X-Real-IP` when there is no `X-Forwarded-For` and
//! `X-Real-IP` has **exactly one line**. **A forwarding header from an
//! untrusted peer is never read.** If the walk meets an entry it cannot
//! parse, finds only trusted proxies, or `X-Real-IP` has several lines, the
//! client is the peer: never an address a client could have chosen.
//! IPv4-mapped IPv6 addresses count as their IPv4 form.
//!
//! The client's bucket is its IPv4 address, or its IPv6 **/64** network (one
//! host controls its whole /64, so a /128 bucket would let it pick buckets).
//!
//! A request with no `ConnectInfo` (a server not built with
//! `into_make_service_with_connect_info`) has no known peer: its headers are
//! not read, all such requests share one client bucket, and the first one
//! logs a warning, once per process.
//!
//! # Memory
//!
//! The client and key maps hold at most `max_tracked_clients` and
//! `max_tracked_keys` buckets. Before a new bucket would grow a map's table,
//! the map is swept of full (expired) buckets, which never changes a
//! decision; a sweep that frees most of the table gives the memory back.
//! When a map is at its cap and full of live buckets, a new client or key
//! shares one **overflow** bucket for that dimension, at the same rate: it
//! is never admitted unlimited. At the cap a map is swept at most once per
//! second. The overflow is logged at most once per minute, by an
//! overflowing request, as a count of the overflowing requests since the
//! last line (no address, no key).
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
//! network only (IPv4 /24, IPv6 /64), and a per-dimension counter
//! ([`RateLimiter::refusals`]). When several limits refuse, the binding one is
//! the one with the longest wait (ties: global, then client, then key), and
//! `Retry-After` is that wait.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// While a map is at its cap, it is swept at most this often.
const CAPPED_SWEEP_GAP_NS: u64 = NANOS_PER_SECOND;

/// The overflow is logged at most this often, as a count.
const OVERFLOW_LOG_GAP_NS: u64 = NANOS_PER_MINUTE;

/// Set by the first request that reaches an enabled limiter with no peer.
static NO_PEER_WARNED: AtomicBool = AtomicBool::new(false);

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

/// The bucket maps' size now: entries held and allocated capacity (the
/// number of entries the table can hold without growing).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tracked {
    pub clients: usize,
    pub client_capacity: usize,
    pub keys: usize,
    pub key_capacity: usize,
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

/// The bucket a client is limited in: an IPv4 address, an IPv6 /64, or the
/// one bucket for requests with no known peer. 16 bytes. Deliberately not
/// `Debug`, so it cannot be logged by accident.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ClientKey {
    V4(u32),
    V6Net(u64),
    Unknown,
}

impl ClientKey {
    fn of(client: Option<IpAddr>) -> Self {
        match client.map(|ip| ip.to_canonical()) {
            Some(IpAddr::V4(v4)) => Self::V4(u32::from(v4)),
            Some(IpAddr::V6(v6)) => Self::V6Net((u128::from(v6) >> 64) as u64),
            None => Self::Unknown,
        }
    }

    /// The network for logs: IPv4 /24, IPv6 /64. Never a full address.
    fn log_network(self) -> String {
        match self {
            Self::V4(addr) => {
                let [a, b, c, _] = Ipv4Addr::from(addr).octets();
                format!("{a}.{b}.{c}.0/24")
            }
            Self::V6Net(prefix) => format!("{}/64", Ipv6Addr::from(u128::from(prefix) << 64)),
            Self::Unknown => "unknown".to_string(),
        }
    }
}

/// Where a request's bucket lives in a map. Not `Debug` (it holds a client
/// or a key digest).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot<K> {
    /// The key's own bucket (new or existing).
    Tracked(K),
    /// The map is at its cap and full of live buckets: the shared overflow
    /// bucket, at the same rate.
    Overflow,
}

/// Arrival times per bucket key, at most `cap` of them. An entry whose time
/// has passed is a full bucket, the same as no entry, so sweeping it changes
/// nothing.
struct Buckets<K> {
    tats: HashMap<K, u64>,
    cap: usize,
    overflow: Option<u64>,
    next_capped_sweep_at: u64,
    overflow_pending: u64,
    overflow_logged_at: Option<u64>,
    sweeps: u64,
}

impl<K: Hash + Eq + Copy> Buckets<K> {
    /// `cap` 0 behaves as 1.
    fn new(cap: u32) -> Self {
        Self {
            tats: HashMap::new(),
            cap: usize::try_from(cap).unwrap_or(usize::MAX).max(1),
            overflow: None,
            next_capped_sweep_at: 0,
            overflow_pending: 0,
            overflow_logged_at: None,
            sweeps: 0,
        }
    }

    /// Where `key`'s request is decided. A key already held keeps its own
    /// bucket. A new key is held unless the map is at its cap with only live
    /// buckets; then it shares the overflow bucket. Sweeps run here, before
    /// any insert: when the new key would grow the table, and (at most once
    /// per [`CAPPED_SWEEP_GAP_NS`]) when the map is at its cap.
    fn slot(&mut self, key: K, now: u64) -> Slot<K> {
        if self.tats.contains_key(&key) {
            return Slot::Tracked(key);
        }
        if self.tats.len() >= self.cap {
            if now >= self.next_capped_sweep_at {
                self.sweep(now);
                self.next_capped_sweep_at = now.saturating_add(CAPPED_SWEEP_GAP_NS);
            }
            if self.tats.len() >= self.cap {
                self.overflow_pending = self.overflow_pending.saturating_add(1);
                return Slot::Overflow;
            }
        } else if self.tats.len() >= self.tats.capacity() {
            self.sweep(now);
        }
        Slot::Tracked(key)
    }

    fn tat(&self, slot: &Slot<K>) -> Option<u64> {
        match slot {
            Slot::Tracked(key) => self.tats.get(key).copied(),
            Slot::Overflow => self.overflow,
        }
    }

    fn commit(&mut self, slot: Slot<K>, tat: u64) {
        match slot {
            Slot::Tracked(key) => {
                self.tats.insert(key, tat);
            }
            Slot::Overflow => self.overflow = Some(tat),
        }
    }

    /// Drop the full buckets. If that freed most of the table, give the
    /// memory back; if not, grow the table now (never past the cap), so the
    /// next sweep is at least as many inserts away as the map now holds.
    ///
    /// The table's size is read before `retain`: erasing from a full table
    /// leaves tombstones, so `capacity()` afterwards understates it.
    fn sweep(&mut self, now: u64) {
        self.sweeps = self.sweeps.saturating_add(1);
        let allocated = self.tats.capacity();
        self.tats.retain(|_, tat| *tat > now);
        let len = self.tats.len();
        if len.saturating_mul(2) < allocated {
            self.tats.shrink_to(len.saturating_add(len / 2));
        } else {
            let room = self.cap.saturating_sub(len).min(len.max(1));
            self.tats.reserve(room);
        }
    }

    /// The overflow count to log, at most once per [`OVERFLOW_LOG_GAP_NS`].
    fn overflow_report(&mut self, now: u64) -> Option<u64> {
        if self.overflow_pending == 0 {
            return None;
        }
        if let Some(at) = self.overflow_logged_at {
            if now.saturating_sub(at) < OVERFLOW_LOG_GAP_NS {
                return None;
            }
        }
        self.overflow_logged_at = Some(now);
        Some(std::mem::take(&mut self.overflow_pending))
    }
}

struct State {
    global: Option<u64>,
    clients: Buckets<ClientKey>,
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
    client: ClientKey,
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
                clients: Buckets::new(config.max_tracked_clients),
                keys: Buckets::new(config.max_tracked_keys),
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

    /// The bucket maps' size now.
    pub fn tracked(&self) -> Tracked {
        let state = self.state.lock();
        Tracked {
            clients: state.clients.tats.len(),
            client_capacity: state.clients.tats.capacity(),
            keys: state.keys.tats.len(),
            key_capacity: state.keys.tats.capacity(),
        }
    }

    /// The client address a request from socket peer `peer` is limited as
    /// (see the module docs; its bucket is this address, or its /64 for
    /// IPv6). `None` only when the peer is unknown.
    pub fn client_ip(&self, peer: Option<IpAddr>, headers: &HeaderMap) -> Option<IpAddr> {
        let peer = peer?.to_canonical();
        if !self.trusted_proxies.contains(&peer) {
            return Some(peer);
        }
        let forwarded: Vec<&HeaderValue> = headers.get_all("x-forwarded-for").iter().collect();
        if !forwarded.is_empty() {
            return Some(self.rightmost_untrusted(&forwarded).unwrap_or(peer));
        }
        let mut real_ip = headers.get_all("x-real-ip").iter();
        let real = match (real_ip.next(), real_ip.next()) {
            (Some(only), None) => only.to_str().ok().and_then(parse_ip),
            _ => None,
        };
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
        if peer.is_none() && !NO_PEER_WARNED.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "rate limiter: a request arrived with no peer address (no ConnectInfo<SocketAddr>); \
                 serve the router with into_make_service_with_connect_info::<SocketAddr>(). Until \
                 then every such request shares one client bucket. Logged once per process."
            );
        }
        let client = ClientKey::of(self.client_ip(peer, req.headers()));
        let key = if self.key.is_some() {
            key_digest(req.headers())
        } else {
            None
        };
        let now = u64::try_from(self.clock.now().as_nanos()).unwrap_or(u64::MAX);

        let mut state = self.state.lock();
        let global = self.global.map(|rate| rate.check(now, state.global));
        let per_client = match self.client {
            Some(rate) => {
                let slot = state.clients.slot(client, now);
                Some((slot, rate.check(now, state.clients.tat(&slot))))
            }
            None => None,
        };
        let per_key = match (self.key, key) {
            (Some(rate), Some(digest)) => {
                let slot = state.keys.slot(digest, now);
                Some((slot, rate.check(now, state.keys.tat(&slot))))
            }
            _ => None,
        };

        let mut binding: Option<(Dimension, u64)> = None;
        for (dimension, outcome) in [
            (Dimension::Global, global),
            (Dimension::Client, per_client.map(|(_, outcome)| outcome)),
            (Dimension::Key, per_key.map(|(_, outcome)| outcome)),
        ] {
            if let Some(Err(wait_ns)) = outcome {
                if binding.is_none_or(|(_, longest)| wait_ns > longest) {
                    binding = Some((dimension, wait_ns));
                }
            }
        }

        let refusal = match binding {
            Some((dimension, wait_ns)) => Some(Refusal {
                dimension,
                wait_ns,
                client,
            }),
            None => {
                if let Some(Ok(tat)) = global {
                    state.global = Some(tat);
                }
                if let Some((slot, Ok(tat))) = per_client {
                    state.clients.commit(slot, tat);
                }
                if let Some((slot, Ok(tat))) = per_key {
                    state.keys.commit(slot, tat);
                }
                None
            }
        };
        // The overflow is reported by an overflowing request only, so each
        // line's count runs up to and including the request that logs it.
        let client_report = if matches!(per_client, Some((Slot::Overflow, _))) {
            state.clients.overflow_report(now)
        } else {
            None
        };
        let key_report = if matches!(per_key, Some((Slot::Overflow, _))) {
            state.keys.overflow_report(now)
        } else {
            None
        };
        let overflow = [(Dimension::Client, client_report), (Dimension::Key, key_report)];
        drop(state);

        for (dimension, report) in overflow {
            if let Some(requests) = report {
                tracing::warn!(
                    dimension = dimension.as_str(),
                    requests,
                    "rate limiter: the bucket map is at its cap with only live buckets; new \
                     arrivals share one overflow bucket at the same rate (requests since the \
                     last report)"
                );
            }
        }
        if let Some(refusal) = &refusal {
            self.counter(refusal.dimension)
                .fetch_add(1, Ordering::Relaxed);
        }
        refusal
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

fn refusal_response(refusal: &Refusal) -> Response {
    let secs = retry_after_secs(refusal.wait_ns);
    tracing::warn!(
        dimension = refusal.dimension.as_str(),
        client_net = %refusal.client.log_network(),
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
    let meant = if per_second > 0 && burst == 0 {
        // tower_governor's GovernorConfigBuilder::finish() returns None for a
        // burst of 0, and the old main.rs called .expect() on it.
        format!(
            "with rate_limit_burst = 0 the old binary panicked at boot (tower_governor's \
             finish() returned None), so this pair never ran; rate_limit_per_second = \
             {per_second} would have meant one request every {per_second} s per peer IP"
        )
    } else if per_second > 0 {
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
    fn client_keys_and_logged_networks() {
        let v4 = ClientKey::of(Some("203.0.113.7".parse().unwrap()));
        assert_eq!(v4.log_network(), "203.0.113.0/24");
        assert!(v4 != ClientKey::of(Some("203.0.113.8".parse().unwrap())));
        let mapped = ClientKey::of(Some("::ffff:203.0.113.7".parse().unwrap()));
        assert!(mapped == v4, "IPv4-mapped IPv6 is IPv4");
        let a = ClientKey::of(Some("2001:db8:1:2::1".parse().unwrap()));
        let b = ClientKey::of(Some("2001:db8:1:2:ffff:ffff:ffff:ffff".parse().unwrap()));
        let c = ClientKey::of(Some("2001:db8:1:3::1".parse().unwrap()));
        assert!(a == b, "one /64");
        assert!(a != c, "another /64");
        assert_eq!(a.log_network(), "2001:db8:1:2::/64");
        assert_eq!(ClientKey::of(None).log_network(), "unknown");
    }

    /// The worst-case memory in the config docs rests on these sizes:
    /// hashbrown holds `size_of::<(K, V)>() + 1` bytes per bucket.
    #[test]
    fn entry_sizes_behind_the_memory_bound() {
        assert_eq!(std::mem::size_of::<(ClientKey, u64)>(), 24);
        assert_eq!(std::mem::size_of::<([u8; 32], u64)>(), 40);
    }

    #[test]
    fn the_cap_holds_and_overflow_shares_one_bucket() {
        let rate = Rate::new(60, 1).expect("on");
        let mut b: Buckets<u32> = Buckets::new(4);
        for k in 0..4 {
            let slot = b.slot(k, 0);
            assert!(slot == Slot::Tracked(k));
            b.commit(slot, rate.check(0, b.tat(&slot)).expect("fresh"));
        }
        let slot = b.slot(10, 0);
        assert!(slot == Slot::Overflow, "full of live buckets");
        b.commit(slot, rate.check(0, b.tat(&slot)).expect("overflow's token"));
        let slot = b.slot(11, 0);
        assert!(slot == Slot::Overflow);
        assert!(rate.check(0, b.tat(&slot)).is_err(), "the overflow is limited");
        assert_eq!(b.tats.len(), 4, "the cap holds");
        assert!(
            b.slot(2, 0) == Slot::Tracked(2),
            "a held key keeps its bucket"
        );
    }

    #[test]
    fn at_the_cap_sweeps_are_at_most_once_a_second() {
        let mut b: Buckets<u32> = Buckets::new(8);
        for k in 0..8 {
            let slot = b.slot(k, 0);
            b.commit(slot, 5 * NANOS_PER_SECOND);
        }
        let before = b.sweeps;
        for k in 100..10_100 {
            assert!(b.slot(k, 0) == Slot::Overflow);
        }
        assert_eq!(b.sweeps, before + 1, "one sweep for 10 000 overflowing arrivals");
        assert!(b.slot(20_000, NANOS_PER_SECOND) == Slot::Overflow);
        assert_eq!(b.sweeps, before + 2, "a second later, one more");
        assert!(
            b.slot(20_001, 5 * NANOS_PER_SECOND) == Slot::Tracked(20_001),
            "expired buckets are swept before a new one is held"
        );
        assert_eq!(b.tats.len(), 0);
    }

    #[test]
    fn sweeps_below_the_cap_are_amortised() {
        let mut b: Buckets<u32> = Buckets::new(1_000_000);
        for k in 0..100_000 {
            let slot = b.slot(k, 0);
            b.commit(slot, NANOS_PER_SECOND);
        }
        assert!(
            b.sweeps <= 20,
            "a sweep only when the table would grow: {} sweeps for 100 000 inserts",
            b.sweeps
        );
    }

    #[test]
    fn a_sweep_that_frees_most_of_the_table_returns_its_memory() {
        let mut b: Buckets<u32> = Buckets::new(1_000_000);
        for k in 0..50_000 {
            let slot = b.slot(k, 0);
            b.commit(slot, NANOS_PER_SECOND);
        }
        let capacity = b.tats.capacity();
        b.sweep(2 * NANOS_PER_SECOND);
        assert_eq!(b.tats.len(), 0);
        assert!(
            b.tats.capacity() < capacity / 8,
            "{} -> {}",
            capacity,
            b.tats.capacity()
        );
    }
}
