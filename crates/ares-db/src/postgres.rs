use crate::query_builders::{
    message_role_from_db, message_role_to_db, DELETE_SESSION_BY_ID_SQL,
    DELETE_SESSION_BY_TOKEN_SQL, INSERT_MESSAGE_SQL, SELECT_MESSAGES_SQL,
    VALIDATE_SESSION_SQL,
};
use ares_types::types::{AppError, MemoryFact, Message, MessageRole, Preference, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;

/// Default PostgreSQL connection URL used when no override or env var is set.
pub const DEFAULT_POSTGRES_URL: &str = "postgres://postgres:postgres@localhost:5432/ares";

/// Lazy pool URL for `PostgresClient::new_test()` (no live connection).
pub const TEST_POSTGRES_URL: &str = "postgres://test:test@localhost:5432/test";

/// Default pool size for production connections.
pub const DEFAULT_MAX_CONNECTIONS: u32 = 5;

/// Returns the default PostgreSQL connection URL.
pub fn default_postgres_url() -> String {
    DEFAULT_POSTGRES_URL.to_string()
}

/// Returns the default max connection pool size.
pub fn default_max_connections() -> u32 {
    DEFAULT_MAX_CONNECTIONS
}

/// Resolve a database URL from an explicit override, `DATABASE_URL`, or [`default_postgres_url`].
pub fn resolve_database_url(override_url: Option<&str>) -> String {
    if let Some(url) = override_url {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    std::env::var("DATABASE_URL").unwrap_or_else(|_| default_postgres_url())
}

/// Parsed components of a `postgres://` or `postgresql://` connection string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresUrlParts {
    pub scheme: String,
    pub user: Option<String>,
    pub password: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub database: String,
}

/// Parse a PostgreSQL connection URL into its components.
pub fn parse_postgres_url(url: &str) -> std::result::Result<PostgresUrlParts, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("empty connection url".to_string());
    }

    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| "missing url scheme".to_string())?;

    if scheme != "postgres" && scheme != "postgresql" {
        return Err(format!("unsupported scheme: {scheme}"));
    }

    let rest = rest.split('?').next().unwrap_or(rest);
    let (authority, database) = rest
        .split_once('/')
        .filter(|(_, db)| !db.is_empty())
        .ok_or_else(|| "missing database name".to_string())?;

    let (user, password, host_port) = match authority.split_once('@') {
        Some((userinfo, hp)) => {
            let (user, password) = match userinfo.split_once(':') {
                Some((u, p)) => (Some(u.to_string()), Some(p.to_string())),
                None => (Some(userinfo.to_string()), None),
            };
            (user, password, hp)
        }
        None => (None, None, authority),
    };

    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && !h.contains('/') => {
            let port = p
                .parse::<u16>()
                .map_err(|_| format!("invalid port: {p}"))?;
            (h.to_string(), Some(port))
        }
        _ => (host_port.to_string(), None),
    };

    if host.is_empty() {
        return Err("missing host".to_string());
    }

    Ok(PostgresUrlParts {
        scheme: scheme.to_string(),
        user,
        password,
        host,
        port,
        database: database.to_string(),
    })
}

/// Returns `true` when [`parse_postgres_url`] succeeds.
pub fn is_postgres_url(url: &str) -> bool {
    parse_postgres_url(url).is_ok()
}

/// PostgreSQL client configuration (URL + pool size).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostgresConfig {
    #[serde(default = "default_postgres_url")]
    pub url: String,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

impl Default for PostgresConfig {
    fn default() -> Self {
        Self {
            url: default_postgres_url(),
            max_connections: default_max_connections(),
        }
    }
}

fn default_max_tool_iterations() -> i32 {
    10
}

/// JSON configuration stored in `UserAgent::extra`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UserAgentExtraConfig {
    #[serde(default)]
    pub parallel_tools: bool,
    #[serde(default = "default_max_tool_iterations")]
    pub max_tool_iterations: i32,
    #[serde(default)]
    pub custom: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Conversation {
    pub id: String,
    pub user_id: String,
    pub title: Option<String>,
    #[sqlx(default)]
    pub message_count: i32,
    pub created_at: String,
    pub updated_at: String,
}

pub struct PostgresClient {
    pub pool: PgPool,
}

impl PostgresClient {
    pub async fn new_remote(url: String, _auth_token: String) -> Result<Self> {
        let config = PostgresConfig {
            url,
            max_connections: default_max_connections(),
        };
        Self::connect_with_config(&config).await
    }

    async fn connect_with_config(config: &PostgresConfig) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .connect(&config.url)
            .await
            .map_err(|e| AppError::Database(format!("Failed to connect to Postgres: {}", e)))?;
        Ok(Self { pool })
    }

    pub async fn new_local(_path: &str) -> Result<Self> {
        let url = resolve_database_url(None);
        Self::new_remote(url, "".to_string()).await
    }

    pub async fn new_memory() -> Result<Self> {
        Self::new_local("").await
    }

    /// Create a test-only client with a lazy pool that doesn't actually connect.
    /// Use this in unit tests that construct AppState but never execute queries.
    #[doc(hidden)]
    pub fn new_test() -> Self {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy(TEST_POSTGRES_URL)
            .expect("connect_lazy should never fail");
        Self { pool }
    }

    pub async fn new(url: String, auth_token: String) -> Result<Self> {
        Self::new_remote(url, auth_token).await
    }

    pub async fn operation_conn(&self) -> Result<&PgPool> {
        Ok(&self.pool)
    }

    pub async fn create_user(
        &self,
        id: &str,
        email: &str,
        password_hash: &str,
        name: &str,
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        sqlx::query("INSERT INTO users (id, email, password_hash, name, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6)")
            .bind(id).bind(email).bind(password_hash).bind(name).bind(now).bind(now).execute(&self.pool).await
            .map_err(|e| AppError::Database(format!("Failed to create user: {}", e)))?;
        Ok(())
    }

    pub async fn get_user_by_email(&self, email: &str) -> Result<Option<User>> {
        sqlx::query_as::<_, User>("SELECT id, email, password_hash, name, created_at, updated_at FROM users WHERE email = $1")
            .bind(email).fetch_optional(&self.pool).await
            .map_err(|e| AppError::Database(format!("Failed to query user: {}", e)))
    }

    pub async fn get_user_by_id(&self, id: &str) -> Result<Option<User>> {
        sqlx::query_as::<_, User>("SELECT id, email, password_hash, name, created_at, updated_at FROM users WHERE id = $1")
            .bind(id).fetch_optional(&self.pool).await
            .map_err(|e| AppError::Database(format!("Failed to query user: {}", e)))
    }

    pub async fn create_session(
        &self,
        id: &str,
        user_id: &str,
        token_hash: &str,
        expires_at: i64,
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        sqlx::query("INSERT INTO sessions (id, user_id, token_hash, expires_at, created_at) VALUES ($1, $2, $3, $4, $5)")
            .bind(id).bind(user_id).bind(token_hash).bind(expires_at).bind(now).execute(&self.pool).await
            .map_err(|e| AppError::Database(format!("Failed to create session: {}", e)))?;
        Ok(())
    }

    pub async fn validate_session(&self, token_hash: &str) -> Result<Option<String>> {
        let now = Utc::now().timestamp();
        let row: Option<(String,)> = sqlx::query_as(VALIDATE_SESSION_SQL)
        .bind(token_hash)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| AppError::Database(format!("Failed to validate session: {}", e)))?;
        Ok(row.map(|(id,)| id))
    }

    pub async fn delete_session(&self, id: &str) -> Result<()> {
        sqlx::query(DELETE_SESSION_BY_ID_SQL)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Database(format!("Failed to delete session: {}", e)))?;
        Ok(())
    }

    pub async fn delete_session_by_token_hash(&self, token_hash: &str) -> Result<()> {
        sqlx::query(DELETE_SESSION_BY_TOKEN_SQL)
            .bind(token_hash)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Database(format!("Failed to delete session: {}", e)))?;
        Ok(())
    }

    pub async fn create_conversation(
        &self,
        id: &str,
        user_id: &str,
        title: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        sqlx::query("INSERT INTO conversations (id, user_id, title, created_at, updated_at) VALUES ($1, $2, $3, $4, $5)")
            .bind(id).bind(user_id).bind(title).bind(now).bind(now).execute(&self.pool).await
            .map_err(|e| AppError::Database(format!("Failed to create conversation: {}", e)))?;
        Ok(())
    }

    pub async fn conversation_exists(&self, conversation_id: &str) -> Result<bool> {
        let row: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM conversations WHERE id = $1")
            .bind(conversation_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| AppError::Database(format!("Failed to check conversation: {}", e)))?;
        Ok(row.is_some())
    }

    pub async fn get_user_conversations(
        &self,
        user_id: &str,
    ) -> Result<Vec<crate::traits::ConversationSummary>> {
        let rows = sqlx::query_as::<_, crate::traits::ConversationSummary>(
            "SELECT c.id, COALESCE(c.title, '') as title, c.created_at, c.updated_at, (SELECT COUNT(*) FROM messages WHERE conversation_id = c.id) as message_count FROM conversations c WHERE c.user_id = $1 ORDER BY c.updated_at DESC"
        )
        .bind(user_id).fetch_all(&self.pool).await
        .map_err(|e| AppError::Database(format!("Failed to query conversations: {}", e)))?;
        Ok(rows)
    }

    pub async fn add_message(
        &self,
        id: &str,
        conversation_id: &str,
        role: MessageRole,
        content: &str,
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        let role_str = message_role_to_db(&role);
        sqlx::query(INSERT_MESSAGE_SQL)
            .bind(id).bind(conversation_id).bind(role_str).bind(content).bind(now).execute(&self.pool).await
            .map_err(|e| AppError::Database(format!("Failed to add message: {}", e)))?;
        Ok(())
    }

    pub async fn get_conversation_history(&self, conversation_id: &str) -> Result<Vec<Message>> {
        #[derive(sqlx::FromRow)]
        struct MessageRow {
            role: String,
            content: String,
            timestamp: i64,
        }
        let rows = sqlx::query_as::<_, MessageRow>(SELECT_MESSAGES_SQL)
            .bind(conversation_id).fetch_all(&self.pool).await.map_err(|e| AppError::Database(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| Message {
                role: message_role_from_db(&row.role),
                content: row.content,
                timestamp: DateTime::from_timestamp(row.timestamp, 0).unwrap_or_default(),
            })
            .collect())
    }

    pub async fn store_memory_fact(&self, fact: &MemoryFact) -> Result<()> {
        sqlx::query("INSERT INTO memory_facts (id, user_id, category, fact_key, fact_value, confidence, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT(id) DO UPDATE SET fact_value = $5")
            .bind(&fact.id).bind(&fact.user_id).bind(&fact.category).bind(&fact.fact_key).bind(&fact.fact_value).bind(fact.confidence as f64).bind(fact.created_at.timestamp()).bind(fact.updated_at.timestamp()).execute(&self.pool).await
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub async fn get_user_memory(&self, user_id: &str) -> Result<Vec<MemoryFact>> {
        #[derive(sqlx::FromRow)]
        struct MemRow {
            id: String,
            user_id: String,
            category: String,
            fact_key: String,
            fact_value: String,
            confidence: f64,
            created_at: i64,
            updated_at: i64,
        }
        let rows = sqlx::query_as::<_, MemRow>("SELECT * FROM memory_facts WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| MemoryFact {
                id: row.id,
                user_id: row.user_id,
                category: row.category,
                fact_key: row.fact_key,
                fact_value: row.fact_value,
                confidence: row.confidence as f32,
                created_at: DateTime::from_timestamp(row.created_at, 0).unwrap_or_default(),
                updated_at: DateTime::from_timestamp(row.updated_at, 0).unwrap_or_default(),
            })
            .collect())
    }

    pub async fn store_preference(&self, user_id: &str, preference: &Preference) -> Result<()> {
        let now = Utc::now().timestamp();
        let id = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO preferences (id, user_id, category, key, value, confidence, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT(user_id, category, key) DO UPDATE SET value = $5")
            .bind(id).bind(user_id).bind(&preference.category).bind(&preference.key).bind(&preference.value).bind(preference.confidence as f64).bind(now).execute(&self.pool).await
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub async fn get_user_preferences(&self, user_id: &str) -> Result<Vec<Preference>> {
        #[derive(sqlx::FromRow)]
        struct PrefRow {
            category: String,
            key: String,
            value: String,
            confidence: f64,
        }
        let rows = sqlx::query_as::<_, PrefRow>(
            "SELECT category, key, value, confidence FROM preferences WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| Preference {
                category: r.category,
                key: r.key,
                value: r.value,
                confidence: r.confidence as f32,
            })
            .collect())
    }

    pub async fn get_user_agent_by_name(
        &self,
        user_id: &str,
        name: &str,
    ) -> Result<Option<UserAgent>> {
        sqlx::query_as::<_, UserAgent>("SELECT * FROM user_agents WHERE user_id = $1 AND name = $2")
            .bind(user_id)
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| AppError::Database(e.to_string()))
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: String,
    pub email: String,
    pub password_hash: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserAgent {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub model: String,
    pub system_prompt: Option<String>,
    pub tools: String,
    pub max_tool_iterations: i32,
    pub parallel_tools: bool,
    pub extra: String,
    pub is_public: bool,
    pub usage_count: i32,
    pub rating_sum: i32,
    pub rating_count: i32,
    pub created_at: i64,
    pub updated_at: i64,
}

impl UserAgent {
    pub fn new(id: String, user_id: String, name: String, model: String) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            id,
            user_id,
            name,
            display_name: None,
            description: None,
            model,
            system_prompt: None,
            tools: "[]".to_string(),
            max_tool_iterations: 10,
            parallel_tools: false,
            extra: "{}".to_string(),
            is_public: false,
            usage_count: 0,
            rating_sum: 0,
            rating_count: 0,
            created_at: now,
            updated_at: now,
        }
    }
    pub fn tools_vec(&self) -> Vec<String> {
        serde_json::from_str(&self.tools).unwrap_or_default()
    }
    pub fn set_tools(&mut self, tools: Vec<String>) {
        self.tools = serde_json::to_string(&tools).unwrap_or_else(|_| "[]".to_string());
    }
    pub fn average_rating(&self) -> Option<f32> {
        if self.rating_count > 0 {
            Some(self.rating_sum as f32 / self.rating_count as f32)
        } else {
            None
        }
    }

    pub fn extra_config(&self) -> UserAgentExtraConfig {
        serde_json::from_str(&self.extra).unwrap_or_default()
    }

    pub fn set_extra_config(&mut self, config: &UserAgentExtraConfig) {
        self.extra = serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string());
        self.parallel_tools = config.parallel_tools;
        self.max_tool_iterations = config.max_tool_iterations;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Connection string parsing ────────────────────────────────────────

    #[test]
    fn default_postgres_url_matches_constant() {
        assert_eq!(default_postgres_url(), DEFAULT_POSTGRES_URL);
    }

    #[test]
    fn parse_postgres_url_full_credentials() {
        let parts = parse_postgres_url(DEFAULT_POSTGRES_URL).expect("parse default url");
        assert_eq!(parts.scheme, "postgres");
        assert_eq!(parts.user.as_deref(), Some("postgres"));
        assert_eq!(parts.password.as_deref(), Some("postgres"));
        assert_eq!(parts.host, "localhost");
        assert_eq!(parts.port, Some(5432));
        assert_eq!(parts.database, "ares");
        assert!(is_postgres_url(DEFAULT_POSTGRES_URL));
    }

    #[test]
    fn parse_postgres_url_postgresql_scheme() {
        let parts =
            parse_postgres_url("postgresql://app:secret@db.internal:5433/mydb").expect("parse");
        assert_eq!(parts.scheme, "postgresql");
        assert_eq!(parts.user.as_deref(), Some("app"));
        assert_eq!(parts.password.as_deref(), Some("secret"));
        assert_eq!(parts.host, "db.internal");
        assert_eq!(parts.port, Some(5433));
        assert_eq!(parts.database, "mydb");
    }

    #[test]
    fn parse_postgres_url_without_credentials_or_port() {
        let parts = parse_postgres_url("postgres://localhost/ares").expect("parse");
        assert_eq!(parts.user, None);
        assert_eq!(parts.password, None);
        assert_eq!(parts.host, "localhost");
        assert_eq!(parts.port, None);
        assert_eq!(parts.database, "ares");
    }

    #[test]
    fn parse_postgres_url_strips_query_params() {
        let parts =
            parse_postgres_url("postgres://localhost/ares?sslmode=require").expect("parse");
        assert_eq!(parts.database, "ares");
    }

    #[test]
    fn parse_postgres_url_rejects_invalid_input() {
        assert!(parse_postgres_url("").is_err());
        assert!(parse_postgres_url("mysql://localhost/db").is_err());
        assert!(parse_postgres_url("postgres://localhost").is_err());
        assert!(parse_postgres_url("postgres://localhost:abc/db").is_err());
        assert!(!is_postgres_url("not-a-url"));
    }

    #[test]
    fn resolve_database_url_prefers_explicit_override() {
        std::env::remove_var("DATABASE_URL");
        assert_eq!(
            resolve_database_url(Some("postgres://override/db")),
            "postgres://override/db"
        );
    }

    #[test]
    fn resolve_database_url_trims_override() {
        std::env::remove_var("DATABASE_URL");
        assert_eq!(
            resolve_database_url(Some("  postgres://trimmed/db  ")),
            "postgres://trimmed/db"
        );
    }

    #[test]
    fn resolve_database_url_falls_back_to_default_when_env_missing() {
        std::env::remove_var("DATABASE_URL");
        assert_eq!(resolve_database_url(None), default_postgres_url());
    }

    #[test]
    fn resolve_database_url_ignores_blank_override() {
        std::env::remove_var("DATABASE_URL");
        assert_eq!(resolve_database_url(Some("   ")), default_postgres_url());
    }

    #[test]
    fn default_max_connections_matches_constant() {
        assert_eq!(default_max_connections(), DEFAULT_MAX_CONNECTIONS);
    }

    #[test]
    fn resolve_database_url_uses_database_url_env() {
        std::env::set_var("DATABASE_URL", "postgres://env-host/ares");
        assert_eq!(resolve_database_url(None), "postgres://env-host/ares");
        std::env::remove_var("DATABASE_URL");
    }

    #[test]
    fn postgres_url_parts_debug_and_clone() {
        let parts = parse_postgres_url(DEFAULT_POSTGRES_URL).unwrap();
        let cloned = parts.clone();
        assert_eq!(parts, cloned);
        assert!(format!("{:?}", parts).contains("PostgresUrlParts"));
    }

    #[test]
    fn test_postgres_url_matches_new_test_pool() {
        let parts = parse_postgres_url(TEST_POSTGRES_URL).expect("parse test url");
        assert_eq!(parts.database, "test");
        assert_eq!(parts.user.as_deref(), Some("test"));
    }

    // ── Query building helpers (session / message SQL) ───────────────────

    #[test]
    fn validate_session_sql_checks_token_and_expiry() {
        assert!(VALIDATE_SESSION_SQL.contains("token_hash = $1"));
        assert!(VALIDATE_SESSION_SQL.contains("expires_at > $2"));
        assert!(VALIDATE_SESSION_SQL.contains("user_id"));
    }

    #[test]
    fn delete_session_sql_targets_id_and_token() {
        assert!(DELETE_SESSION_BY_ID_SQL.contains("id = $1"));
        assert!(DELETE_SESSION_BY_TOKEN_SQL.contains("token_hash = $1"));
    }

    #[test]
    fn message_sql_helpers_have_expected_placeholders() {
        assert!(INSERT_MESSAGE_SQL.contains("$1"));
        assert!(INSERT_MESSAGE_SQL.contains("conversation_id"));
        assert!(SELECT_MESSAGES_SQL.contains("conversation_id = $1"));
        assert!(SELECT_MESSAGES_SQL.contains("ORDER BY timestamp ASC"));
    }

    #[test]
    fn message_role_round_trip_for_postgres() {
        let cases = [
            (MessageRole::System, "system"),
            (MessageRole::User, "user"),
            (MessageRole::Assistant, "assistant"),
        ];
        for (role, expected) in cases {
            assert_eq!(message_role_to_db(&role), expected);
            assert!(matches!(
                (role, message_role_from_db(expected)),
                (MessageRole::System, MessageRole::System)
                    | (MessageRole::User, MessageRole::User)
                    | (MessageRole::Assistant, MessageRole::Assistant)
            ));
        }
    }

    #[test]
    fn message_role_from_db_defaults_unknown_to_user() {
        assert!(matches!(
            message_role_from_db("human"),
            MessageRole::User
        ));
    }

    // ── Serde: PostgresConfig ──────────────────────────────────────────

    #[test]
    fn postgres_config_default_values() {
        let config = PostgresConfig::default();
        assert_eq!(config.url, default_postgres_url());
        assert_eq!(config.max_connections, DEFAULT_MAX_CONNECTIONS);
    }

    #[test]
    fn postgres_config_serde_roundtrip() {
        let config = PostgresConfig {
            url: "postgres://user:pass@host/db".into(),
            max_connections: 12,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let restored: PostgresConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, config);
    }

    #[test]
    fn postgres_config_deserializes_with_defaults() {
        let json = r#"{"url":"postgres://custom/db"}"#;
        let config: PostgresConfig = serde_json::from_str(json).expect("deserialize");
        assert_eq!(config.url, "postgres://custom/db");
        assert_eq!(config.max_connections, DEFAULT_MAX_CONNECTIONS);
    }

    // ── Serde: UserAgentExtraConfig ────────────────────────────────────

    #[test]
    fn user_agent_extra_config_serde_roundtrip() {
        let mut custom = serde_json::Map::new();
        custom.insert("tier".into(), serde_json::json!("pro"));
        let config = UserAgentExtraConfig {
            parallel_tools: true,
            max_tool_iterations: 25,
            custom,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let restored: UserAgentExtraConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, config);
    }

    #[test]
    fn user_agent_extra_config_deserializes_with_defaults() {
        let config: UserAgentExtraConfig = serde_json::from_str("{}").expect("deserialize");
        assert!(!config.parallel_tools);
        assert_eq!(config.max_tool_iterations, 10);
        assert!(config.custom.is_empty());
    }

    #[test]
    fn user_agent_extra_config_round_trip_on_agent() {
        let mut agent = UserAgent::new(
            "id".into(),
            "uid".into(),
            "bot".into(),
            "gpt".into(),
        );
        let config = UserAgentExtraConfig {
            parallel_tools: true,
            max_tool_iterations: 7,
            custom: serde_json::Map::new(),
        };
        agent.set_extra_config(&config);
        assert_eq!(agent.extra_config(), config);
        assert!(agent.parallel_tools);
        assert_eq!(agent.max_tool_iterations, 7);
    }

    // ── UserAgent helpers ──────────────────────────────────────────────

    #[test]
    fn user_agent_tools_round_trip() {
        let mut agent = UserAgent::new(
            "id".into(),
            "uid".into(),
            "bot".into(),
            "gpt".into(),
        );
        agent.set_tools(vec!["search".into(), "calc".into()]);
        assert_eq!(agent.tools_vec(), vec!["search", "calc"]);
    }

    #[test]
    fn user_agent_average_rating_computed() {
        let mut agent = UserAgent::new(
            "id".into(),
            "uid".into(),
            "bot".into(),
            "gpt".into(),
        );
        assert_eq!(agent.average_rating(), None);
        agent.rating_sum = 9;
        agent.rating_count = 2;
        assert_eq!(agent.average_rating(), Some(4.5));
    }

    /// Regression test: new_test() must not block or hang.
    #[tokio::test]
    async fn test_new_test_does_not_block() {
        let start = std::time::Instant::now();
        let _client = PostgresClient::new_test();
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 100,
            "new_test() should complete instantly, took {}ms",
            elapsed.as_millis()
        );
    }

    /// Regression test: new_test() must work inside #[tokio::test].
    #[tokio::test]
    async fn test_new_test_in_tokio_context() {
        let _client = PostgresClient::new_test();
        // If we get here without hanging, the deadlock is fixed
    }
}
