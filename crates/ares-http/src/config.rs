//! HTTP-facing auth and server configuration (moves to ares-http in Phase 7).

use serde::{Deserialize, Serialize};

// ============= Authentication Configuration =============

/// Authentication configuration settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Environment variable name containing the JWT secret.
    pub jwt_secret_env: String,

    /// JWT access token expiry time in seconds (default: 900 = 15 minutes).
    #[serde(default = "default_jwt_access_expiry")]
    pub jwt_access_expiry: i64,

    /// JWT refresh token expiry time in seconds (default: 604800 = 7 days).
    #[serde(default = "default_jwt_refresh_expiry")]
    pub jwt_refresh_expiry: i64,

    /// Environment variable name containing the API key.
    pub api_key_env: String,
}

fn default_jwt_access_expiry() -> i64 {
    900
}

fn default_jwt_refresh_expiry() -> i64 {
    604800
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret_env: "JWT_SECRET".to_string(),
            jwt_access_expiry: default_jwt_access_expiry(),
            jwt_refresh_expiry: default_jwt_refresh_expiry(),
            api_key_env: "API_KEY".to_string(),
        }
    }
}

// ============= Server Configuration =============

/// Server configuration settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Host address to bind to (default: "127.0.0.1").
    #[serde(default = "default_host")]
    pub host: String,

    /// Port number to listen on (default: 3000).
    #[serde(default = "default_port")]
    pub port: u16,

    /// Log level: "trace", "debug", "info", "warn", "error" (default: "info").
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Allowed CORS origins (default: ["*"] for development, set explicitly for production).
    /// Use specific origins like `["https://yourdomain.com"]` in production.
    #[serde(default = "default_cors_origins")]
    pub cors_origins: Vec<String>,

    /// RETIRED from ARES's own binary: it reads [`ServerConfig::rate_limit`]
    /// instead and logs one startup warning when this key is above 0.
    ///
    /// Under tower_governor, `per_second(n)` meant **one request every `n`
    /// seconds** per peer IP (not `n` per second). Behind a same-host proxy
    /// every request's peer is the proxy, so all clients shared one bucket.
    /// Kept (type and default unchanged) because downstream wrappers still
    /// read it until they switch to [`ServerConfig::rate_limit`].
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_second: u32,

    /// RETIRED from ARES's own binary, with `rate_limit_per_second`: the
    /// tower_governor bucket size, in requests, per peer IP.
    #[serde(default = "default_rate_limit_burst")]
    pub rate_limit_burst: u32,

    /// The `[server.rate_limit]` table: per-client and per-key limits in
    /// requests per minute. Every limit defaults to 0 (off).
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
}

// ============= Rate Limit Configuration =============

/// `[server.rate_limit]`: the limiter in `ares_http::middleware::rate_limit`.
///
/// Units are in every name. Each `*_requests_per_minute` limit is a token
/// bucket that refills at that many requests per minute and holds up to
/// `burst` requests. **0 turns that limit off**, and every limit defaults
/// to 0, so the limiter does nothing until a value is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RateLimitConfig {
    /// One bucket shared by every request (a safety cap), in requests per
    /// minute. 0 = off.
    pub global_requests_per_minute: u32,

    /// One bucket per client, in requests per minute. 0 = off. The client
    /// is the peer IP, unless the peer is in `trusted_proxies` (then it is
    /// taken from `X-Forwarded-For`, else a single-line `X-Real-IP`). An
    /// IPv4 client is its address; an IPv6 client is its /64 network.
    pub per_client_requests_per_minute: u32,

    /// One bucket per `Authorization: Bearer` credential (keyed by its
    /// SHA-256, never the raw value), in requests per minute. 0 = off.
    pub per_key_requests_per_minute: u32,

    /// Bucket size, in requests, for each of the three limits: how many
    /// requests may arrive at once before the per-minute rate applies.
    /// A bucket always holds at least 1 request, so 0 behaves as 1.
    pub burst: u32,

    /// Peers whose `X-Forwarded-For` / `X-Real-IP` headers are read. A
    /// request from any other peer is identified by its peer IP alone.
    /// Exact IP addresses only: a CIDR range fails the config load.
    /// Default: `["127.0.0.1", "::1"]` (a proxy on the same host).
    pub trusted_proxies: Vec<std::net::IpAddr>,

    /// Most client buckets held in memory at once (default 100 000; 0
    /// behaves as 1). An entry is 24 bytes, and the table never grows past
    /// the cap's size: 131 072 slots of 25 bytes, 3 276 816 bytes at the
    /// default. A sweep that frees anything briefly copies the live entries
    /// out (up to 2 400 000 bytes more at the default). When every held
    /// bucket is still live, a new client shares one overflow bucket, at the
    /// same rate, instead of growing the table.
    pub max_tracked_clients: u32,

    /// Most key buckets held in memory at once (default 100 000; 0 behaves
    /// as 1). An entry is 40 bytes: at most 131 072 slots of 41 bytes,
    /// 5 373 968 bytes at the default, plus up to 4 000 000 bytes during a
    /// sweep. Overflow works as for clients.
    pub max_tracked_keys: u32,
}

/// Default for [`RateLimitConfig::max_tracked_clients`] and
/// [`RateLimitConfig::max_tracked_keys`].
pub const DEFAULT_MAX_TRACKED: u32 = 100_000;

impl RateLimitConfig {
    /// True when at least one of the three limits is above 0.
    pub fn any_limit_set(&self) -> bool {
        self.global_requests_per_minute > 0
            || self.per_client_requests_per_minute > 0
            || self.per_key_requests_per_minute > 0
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            global_requests_per_minute: 0,
            per_client_requests_per_minute: 0,
            per_key_requests_per_minute: 0,
            burst: 0,
            trusted_proxies: vec![
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
            ],
            max_tracked_clients: DEFAULT_MAX_TRACKED,
            max_tracked_keys: DEFAULT_MAX_TRACKED,
        }
    }
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    3000
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_cors_origins() -> Vec<String> {
    vec!["http://localhost:3000".to_string()]
}

fn default_rate_limit() -> u32 {
    100
}

fn default_rate_limit_burst() -> u32 {
    10
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            log_level: default_log_level(),
            cors_origins: default_cors_origins(),
            rate_limit_per_second: default_rate_limit(),
            rate_limit_burst: default_rate_limit_burst(),
            rate_limit: RateLimitConfig::default(),
        }
    }
}
