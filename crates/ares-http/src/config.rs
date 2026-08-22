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

    /// Rate limiting: requests per second per IP (default: 100, 0 = disabled).
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_second: u32,

    /// Rate limiting burst size (default: 10).
    #[serde(default = "default_rate_limit_burst")]
    pub rate_limit_burst: u32,
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
        }
    }
}
