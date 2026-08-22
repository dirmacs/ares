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
