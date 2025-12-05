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
    /// Database URL - can be:
    /// - Local file path: "./data/ares.db" or "file:./data/ares.db"
    /// - In-memory: ":memory:"
    /// - Remote Turso: "libsql://your-database.turso.io"
    pub turso_url: String,
    /// Auth token for remote Turso (not needed for local)
    pub turso_auth_token: String,
    /// Qdrant URL (ignored in local mode, kept for compatibility)
    pub qdrant_url: String,
    /// Qdrant API key (ignored in local mode)
    pub qdrant_api_key: Option<String>,
    /// Whether to use local/in-memory databases (default: true)
    pub use_local: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LLMConfig {
    pub openai_api_key: Option<String>,
    pub openai_api_base: Option<String>,
    pub anthropic_api_key: Option<String>,
    pub ollama_url: String,
    /// Default LLM provider: "openai", "ollama", or "llamacpp"
    pub default_provider: Option<String>,
    /// Default model name
    pub default_model: Option<String>,
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

        // Determine if we should use local mode
        let use_local = env::var("USE_LOCAL_DB")
            .map(|v| v.to_lowercase() == "true" || v == "1")
            .unwrap_or(true); // Default to local mode

        // For local mode, use sensible defaults
        let turso_url = if use_local {
            env::var("TURSO_URL").unwrap_or_else(|_| "./data/ares.db".to_string())
        } else {
            env::var("TURSO_URL")?
        };

        let turso_auth_token = if use_local {
            env::var("TURSO_AUTH_TOKEN").unwrap_or_default()
        } else {
            env::var("TURSO_AUTH_TOKEN")?
        };

        // Generate a random JWT secret if not provided (for development)
        let jwt_secret = env::var("JWT_SECRET").unwrap_or_else(|_| {
            use rand::Rng;
            let secret: String = rand::rng()
                .sample_iter(&rand::distr::Alphanumeric)
                .take(32)
                .map(char::from)
                .collect();
            tracing::warn!(
                "JWT_SECRET not set, using randomly generated secret (not suitable for production)"
            );
            secret
        });

        // Generate a random API key if not provided (for development)
        let api_key = env::var("API_KEY").unwrap_or_else(|_| {
            use rand::Rng;
            let key: String = rand::rng()
                .sample_iter(&rand::distr::Alphanumeric)
                .take(32)
                .map(char::from)
                .collect();
            tracing::warn!(
                "API_KEY not set, using randomly generated key (not suitable for production)"
            );
            key
        });

        Ok(Config {
            server: ServerConfig {
                host: env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
                port: env::var("PORT")
                    .unwrap_or_else(|_| "3000".to_string())
                    .parse()?,
            },
            database: DatabaseConfig {
                turso_url,
                turso_auth_token,
                qdrant_url: env::var("QDRANT_URL")
                    .unwrap_or_else(|_| "http://localhost:6334".to_string()),
                qdrant_api_key: env::var("QDRANT_API_KEY").ok(),
                use_local,
            },
            llm: LLMConfig {
                openai_api_key: env::var("OPENAI_API_KEY").ok(),
                openai_api_base: env::var("OPENAI_API_BASE").ok(),
                anthropic_api_key: env::var("ANTHROPIC_API_KEY").ok(),
                ollama_url: env::var("OLLAMA_URL")
                    .unwrap_or_else(|_| "http://localhost:11434".to_string()),
                default_provider: env::var("DEFAULT_LLM_PROVIDER").ok(),
                default_model: env::var("DEFAULT_LLM_MODEL").ok(),
            },
            auth: AuthConfig {
                jwt_secret,
                jwt_access_expiry: env::var("JWT_ACCESS_EXPIRY")
                    .unwrap_or_else(|_| "900".to_string())
                    .parse()?,
                jwt_refresh_expiry: env::var("JWT_REFRESH_EXPIRY")
                    .unwrap_or_else(|_| "604800".to_string())
                    .parse()?,
                api_key,
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

    /// Check if running in local mode (no external services required)
    pub fn is_local_mode(&self) -> bool {
        self.database.use_local
    }

    /// Check if an LLM provider is configured
    pub fn has_llm_provider(&self) -> bool {
        self.llm.openai_api_key.is_some() || self.llm.anthropic_api_key.is_some()
        // Ollama is assumed to be always available locally
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 3000,
            },
            database: DatabaseConfig {
                turso_url: "./data/ares.db".to_string(),
                turso_auth_token: String::new(),
                qdrant_url: "http://localhost:6334".to_string(),
                qdrant_api_key: None,
                use_local: true,
            },
            llm: LLMConfig {
                openai_api_key: None,
                openai_api_base: None,
                anthropic_api_key: None,
                ollama_url: "http://localhost:11434".to_string(),
                default_provider: Some("ollama".to_string()),
                default_model: Some("llama3.2".to_string()),
            },
            auth: AuthConfig {
                jwt_secret: "development-secret-change-in-production".to_string(),
                jwt_access_expiry: 900,
                jwt_refresh_expiry: 604800,
                api_key: "development-api-key".to_string(),
            },
            rag: RAGConfig {
                embedding_model: "BAAI/bge-small-en-v1.5".to_string(),
                chunk_size: 1000,
                chunk_overlap: 200,
            },
        }
    }
}
