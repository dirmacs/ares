use serde::{Deserialize, Serialize};

// ============= Database Configuration =============

/// Database configuration settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// PostgreSQL database URL (default: "postgres://postgres:postgres@localhost:5432/ares").
    #[serde(default = "default_database_url")]
    pub url: String,

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

pub(crate) fn default_qdrant_url() -> String {
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
