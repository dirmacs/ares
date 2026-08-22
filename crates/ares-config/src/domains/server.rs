use serde::{Deserialize, Serialize};

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
