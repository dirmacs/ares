use serde::{Deserialize, Serialize};

// ============= Database Configuration =============

/// Database configuration settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// PostgreSQL database URL (default: "postgres://postgres:postgres@localhost:5432/ares").
    #[serde(default = "default_database_url")]
    pub url: String,

    /// Maximum PostgreSQL pool connections. When unset, the pool resolves
    /// `DATABASE_MAX_CONNECTIONS`, then the built-in default (20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_connections: Option<u32>,

    /// Qdrant vector database configuration (optional).
    pub qdrant: Option<QdrantConfig>,
}

fn default_database_url() -> String {
    "postgres://postgres:postgres@localhost:5432/ares".to_string()
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: default_database_url(),
            max_connections: None,
            qdrant: None,
        }
    }
}

/// Qdrant vector database configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QdrantConfig {
    /// Qdrant server URL (default: "http://localhost:6334").
    #[serde(default = "default_qdrant_url")]
    pub url: String,

    /// Environment variable for Qdrant API key.
    pub api_key_env: Option<String>,
}

pub fn default_qdrant_url() -> String {
    "http://localhost:6334".to_string()
}

impl Default for QdrantConfig {
    fn default() -> Self {
        Self {
            url: default_qdrant_url(),
            api_key_env: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_config_parses_optional_pool_ceiling() {
        let cfg: DatabaseConfig =
            serde_json::from_str(r#"{"url":"postgres://host/db","max_connections":12}"#)
                .expect("parse with pool ceiling");
        assert_eq!(cfg.max_connections, Some(12));

        let cfg: DatabaseConfig = serde_json::from_str(r#"{"url":"postgres://host/db"}"#)
            .expect("parse without pool ceiling");
        assert_eq!(cfg.url, "postgres://host/db");
        assert_eq!(cfg.max_connections, None);
    }
}
