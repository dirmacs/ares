use serde::Deserialize;
use std::env;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub llm: LLMConfig,
    pub auth: AuthConfig,
    pub rag: RAGConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    /// Local database path/URL. Examples:
    /// - "./data/ares.db"
    /// - "file:./data/ares.db"
    /// - ":memory:" (ephemeral)
    pub database_url: String,
    /// Optional Turso/libsql cloud URL (e.g. libsql://<db>-<org>.turso.io)
    pub turso_url: Option<String>,
    /// Optional Turso auth token
    pub turso_auth_token: Option<String>,
    pub qdrant_url: String,
    pub qdrant_api_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LLMConfig {
    pub openai_api_key: Option<String>,
    pub anthropic_api_key: Option<String>,
    pub ollama_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    pub jwt_secret: String,
    pub jwt_access_expiry: i64,
    pub jwt_refresh_expiry: i64,
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RAGConfig {
    pub embedding_model: String,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
}

impl Config {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        dotenv::dotenv().ok();

        Ok(Config {
            server: ServerConfig {
                host: env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
                port: env::var("PORT")
                    .unwrap_or_else(|_| "3000".to_string())
                    .parse()?,
            },
            database: DatabaseConfig {
                // Local-first default
                database_url: env::var("DATABASE_URL")
                    .or_else(|_| env::var("DATABASE_PATH"))
                    .unwrap_or_else(|_| "./data/ares.db".to_string()),
                // Cloud config is optional
                turso_url: env::var("TURSO_URL")
                    .or_else(|_| env::var("TURSO_DATABASE_URL"))
                    .ok()
                    .filter(|v| !v.trim().is_empty()),
                turso_auth_token: env::var("TURSO_AUTH_TOKEN")
                    .ok()
                    .filter(|v| !v.trim().is_empty()),
                qdrant_url: env::var("QDRANT_URL")
                    .unwrap_or_else(|_| "http://localhost:6334".to_string()),
                qdrant_api_key: env::var("QDRANT_API_KEY").ok(),
            },
            llm: LLMConfig {
                openai_api_key: env::var("OPENAI_API_KEY").ok(),
                anthropic_api_key: env::var("ANTHROPIC_API_KEY").ok(),
                ollama_url: env::var("OLLAMA_URL")
                    .unwrap_or_else(|_| "http://localhost:11434".to_string()),
            },
            auth: AuthConfig {
                jwt_secret: env::var("JWT_SECRET")?,
                jwt_access_expiry: env::var("JWT_ACCESS_EXPIRY")
                    .unwrap_or_else(|_| "900".to_string())
                    .parse()?,
                jwt_refresh_expiry: env::var("JWT_REFRESH_EXPIRY")
                    .unwrap_or_else(|_| "604800".to_string())
                    .parse()?,
                api_key: env::var("API_KEY")?,
            },
            rag: RAGConfig {
                embedding_model: env::var("EMBEDDING_MODEL")
                    .unwrap_or_else(|_| "BAAI/bge-small-en-v1.5".to_string()),
                chunk_size: env::var("CHUNK_SIZE")
                    .unwrap_or_else(|_| "1000".to_string())
                    .parse()?,
                chunk_overlap: env::var("CHUNK_OVERLAP")
                    .unwrap_or_else(|_| "200".to_string())
                    .parse()?,
            },
        })
    }
}
