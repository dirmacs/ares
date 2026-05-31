use ares_types::types::{AppError, MemoryFact, Message, MessageRole, Preference, Result};
use async_trait::async_trait;

#[derive(Debug, Clone, Default)]
pub enum DatabaseProvider {
    #[default]
    Memory,
    SQLite {
        path: String,
    },
    Postgres {
        url: String,
    },
    #[cfg(feature = "turso")]
    Turso {
        url: String,
        auth_token: String,
    },
}

impl DatabaseProvider {
    pub async fn create_client(&self) -> Result<Box<dyn DatabaseClient>> {
        match self {
            DatabaseProvider::Memory => {
                let client = super::postgres::PostgresClient::new_memory().await?;
                Ok(Box::new(client))
            }
            DatabaseProvider::SQLite { path } => {
                let client = super::postgres::PostgresClient::new_local(path).await?;
                Ok(Box::new(client))
            }
            DatabaseProvider::Postgres { url } => {
                let client =
                    super::postgres::PostgresClient::new_remote(url.clone(), "".to_string())
                        .await?;
                Ok(Box::new(client))
            }
            #[cfg(feature = "turso")]
            DatabaseProvider::Turso { url, auth_token } => {
                let client = super::turso::TursoClient::new(url.clone(), auth_token.clone()).await?;
                Ok(Box::new(client))
            }
        }
    }

    pub fn from_env() -> Self {
        // Turso takes priority if TURSO_URL is set
        #[cfg(feature = "turso")]
        if let Ok(url) = std::env::var("TURSO_URL") {
            let url = url.trim();
            if !url.is_empty() {
                let token = std::env::var("TURSO_AUTH_TOKEN").unwrap_or_default();
                return DatabaseProvider::Turso {
                    url: url.to_string(),
                    auth_token: token,
                };
            }
        }
        if let Ok(url) = std::env::var("DATABASE_URL") {
            let url = url.trim();
            if !url.is_empty() {
                return DatabaseProvider::Postgres {
                    url: url.to_string(),
                };
            }
        }
        if let Ok(path) = std::env::var("DATABASE_PATH") {
            let path = path.trim();
            if !path.is_empty() && path != ":memory:" {
                return DatabaseProvider::SQLite {
                    path: path.to_string(),
                };
            }
        }
        DatabaseProvider::Memory
    }
}

pub use super::postgres::User;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ConversationSummary {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: i32,
}

#[async_trait]
pub trait DatabaseClient: Send + Sync {
    async fn create_user(
        &self,
        id: &str,
        email: &str,
        password_hash: &str,
        name: &str,
    ) -> Result<()>;
    async fn get_user_by_email(&self, email: &str) -> Result<Option<User>>;
    async fn get_user_by_id(&self, id: &str) -> Result<Option<User>>;
    async fn create_session(
        &self,
        id: &str,
        user_id: &str,
        token_hash: &str,
        expires_at: i64,
    ) -> Result<()>;
    async fn validate_session(&self, token_hash: &str) -> Result<Option<String>>;
    async fn delete_session(&self, id: &str) -> Result<()>;
    async fn delete_session_by_token_hash(&self, token_hash: &str) -> Result<()>;
    async fn create_conversation(&self, id: &str, user_id: &str, title: Option<&str>)
        -> Result<()>;
    async fn conversation_exists(&self, conversation_id: &str) -> Result<bool>;
    async fn get_user_conversations(&self, user_id: &str) -> Result<Vec<ConversationSummary>>;
    async fn get_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<super::postgres::Conversation>;
    async fn delete_conversation(&self, conversation_id: &str) -> Result<()>;
    async fn update_conversation_title(
        &self,
        conversation_id: &str,
        title: Option<&str>,
    ) -> Result<()>;
    async fn add_message(
        &self,
        id: &str,
        conversation_id: &str,
        role: MessageRole,
        content: &str,
    ) -> Result<()>;
    async fn get_conversation_history(&self, conversation_id: &str) -> Result<Vec<Message>>;
    async fn store_memory_fact(&self, fact: &MemoryFact) -> Result<()>;
    async fn get_user_memory(&self, user_id: &str) -> Result<Vec<MemoryFact>>;
    async fn get_memory_by_category(
        &self,
        user_id: &str,
        category: &str,
    ) -> Result<Vec<MemoryFact>>;
    async fn store_preference(&self, user_id: &str, preference: &Preference) -> Result<()>;
    async fn get_user_preferences(&self, user_id: &str) -> Result<Vec<Preference>>;
    async fn get_preference(
        &self,
        user_id: &str,
        category: &str,
        key: &str,
    ) -> Result<Option<Preference>>;
    async fn get_user_agent_by_name(
        &self,
        user_id: &str,
        name: &str,
    ) -> Result<Option<super::postgres::UserAgent>>;
    async fn get_public_agent_by_name(
        &self,
        name: &str,
    ) -> Result<Option<super::postgres::UserAgent>>;
    async fn list_user_agents(&self, user_id: &str) -> Result<Vec<super::postgres::UserAgent>>;
    async fn list_public_agents(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<super::postgres::UserAgent>>;
    async fn create_user_agent(&self, agent: &super::postgres::UserAgent) -> Result<()>;
    async fn update_user_agent(&self, agent: &super::postgres::UserAgent) -> Result<()>;
    async fn delete_user_agent(&self, id: &str, user_id: &str) -> Result<bool>;
}

#[async_trait]
impl DatabaseClient for super::postgres::PostgresClient {
    async fn create_user(
        &self,
        id: &str,
        email: &str,
        password_hash: &str,
        name: &str,
    ) -> Result<()> {
        super::postgres::PostgresClient::create_user(self, id, email, password_hash, name).await
    }
    async fn get_user_by_email(&self, email: &str) -> Result<Option<User>> {
        super::postgres::PostgresClient::get_user_by_email(self, email).await
    }
    async fn get_user_by_id(&self, id: &str) -> Result<Option<User>> {
        super::postgres::PostgresClient::get_user_by_id(self, id).await
    }
    async fn create_session(
        &self,
        id: &str,
        user_id: &str,
        token_hash: &str,
        expires_at: i64,
    ) -> Result<()> {
        super::postgres::PostgresClient::create_session(self, id, user_id, token_hash, expires_at)
            .await
    }
    async fn validate_session(&self, token_hash: &str) -> Result<Option<String>> {
        super::postgres::PostgresClient::validate_session(self, token_hash).await
    }
    async fn delete_session(&self, id: &str) -> Result<()> {
        super::postgres::PostgresClient::delete_session(self, id).await
    }
    async fn delete_session_by_token_hash(&self, token_hash: &str) -> Result<()> {
        super::postgres::PostgresClient::delete_session_by_token_hash(self, token_hash).await
    }
    async fn create_conversation(
        &self,
        id: &str,
        user_id: &str,
        title: Option<&str>,
    ) -> Result<()> {
        super::postgres::PostgresClient::create_conversation(self, id, user_id, title).await
    }
    async fn conversation_exists(&self, conversation_id: &str) -> Result<bool> {
        super::postgres::PostgresClient::conversation_exists(self, conversation_id).await
    }
    async fn get_user_conversations(&self, user_id: &str) -> Result<Vec<ConversationSummary>> {
        super::postgres::PostgresClient::get_user_conversations(self, user_id).await
    }
    async fn get_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<super::postgres::Conversation> {
        let row = sqlx::query_as::<_, super::postgres::Conversation>("SELECT id, user_id, title, created_at, updated_at, 0 as message_count FROM conversations WHERE id = $1").bind(conversation_id).fetch_optional(&self.pool).await.map_err(|e| AppError::Database(e.to_string()))?;
        row.ok_or_else(|| AppError::NotFound("Conversation not found".into()))
    }
    async fn delete_conversation(&self, conversation_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM messages WHERE conversation_id = $1")
            .bind(conversation_id)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Database(e.to_string()))?;
        sqlx::query("DELETE FROM conversations WHERE id = $1")
            .bind(conversation_id)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }
    async fn update_conversation_title(
        &self,
        conversation_id: &str,
        title: Option<&str>,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query("UPDATE conversations SET title = $1, updated_at = $2 WHERE id = $3")
            .bind(title)
            .bind(now)
            .bind(conversation_id)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }
    async fn add_message(
        &self,
        id: &str,
        conversation_id: &str,
        role: MessageRole,
        content: &str,
    ) -> Result<()> {
        super::postgres::PostgresClient::add_message(self, id, conversation_id, role, content).await
    }
    async fn get_conversation_history(&self, conversation_id: &str) -> Result<Vec<Message>> {
        super::postgres::PostgresClient::get_conversation_history(self, conversation_id).await
    }
    async fn store_memory_fact(&self, fact: &MemoryFact) -> Result<()> {
        super::postgres::PostgresClient::store_memory_fact(self, fact).await
    }
    async fn get_user_memory(&self, user_id: &str) -> Result<Vec<MemoryFact>> {
        super::postgres::PostgresClient::get_user_memory(self, user_id).await
    }
    async fn get_memory_by_category(
        &self,
        user_id: &str,
        category: &str,
    ) -> Result<Vec<MemoryFact>> {
        let mems = super::postgres::PostgresClient::get_user_memory(self, user_id).await?;
        Ok(mems
            .into_iter()
            .filter(|m| m.category == category)
            .collect())
    }
    async fn store_preference(&self, user_id: &str, preference: &Preference) -> Result<()> {
        super::postgres::PostgresClient::store_preference(self, user_id, preference).await
    }
    async fn get_user_preferences(&self, user_id: &str) -> Result<Vec<Preference>> {
        super::postgres::PostgresClient::get_user_preferences(self, user_id).await
    }
    async fn get_preference(
        &self,
        user_id: &str,
        category: &str,
        key: &str,
    ) -> Result<Option<Preference>> {
        let prefs = super::postgres::PostgresClient::get_user_preferences(self, user_id).await?;
        Ok(prefs
            .into_iter()
            .find(|p| p.category == category && p.key == key))
    }
    async fn get_user_agent_by_name(
        &self,
        user_id: &str,
        name: &str,
    ) -> Result<Option<super::postgres::UserAgent>> {
        super::postgres::PostgresClient::get_user_agent_by_name(self, user_id, name).await
    }
    async fn get_public_agent_by_name(
        &self,
        name: &str,
    ) -> Result<Option<super::postgres::UserAgent>> {
        super::postgres::PostgresClient::get_user_agent_by_name(self, "", name).await
    }
    async fn list_user_agents(&self, _user_id: &str) -> Result<Vec<super::postgres::UserAgent>> {
        Ok(vec![])
    }
    async fn list_public_agents(
        &self,
        _limit: u32,
        _offset: u32,
    ) -> Result<Vec<super::postgres::UserAgent>> {
        Ok(vec![])
    }
    async fn create_user_agent(&self, _agent: &super::postgres::UserAgent) -> Result<()> {
        Ok(())
    }
    async fn update_user_agent(&self, _agent: &super::postgres::UserAgent) -> Result<()> {
        Ok(())
    }
    async fn delete_user_agent(&self, _id: &str, _user_id: &str) -> Result<bool> {
        Ok(true)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::postgres::{PostgresClient, UserAgent};
    use ares_types::types::{MemoryFact, MessageRole, Preference};
    use sqlx::postgres::PgPoolOptions;
    use sqlx::PgPool;

    fn unreachable_postgres_pool() -> PgPool {
        PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/nope")
            .expect("connect_lazy should not fail for malformed URLs")
    }

    fn unreachable_client() -> PostgresClient {
        PostgresClient {
            pool: unreachable_postgres_pool(),
        }
    }

    fn assert_database_error<T: std::fmt::Debug>(result: Result<T>) {
        matches::assert_matches!(
            result.unwrap_err(),
            AppError::Database(msg) if !msg.is_empty()
        );
    }

    #[test]
    fn test_database_provider_default_returns_memory() {
        let provider = DatabaseProvider::default();
        matches::assert_matches!(provider, DatabaseProvider::Memory);
    }
    #[test]
    fn test_database_provider_debug_format_memory() {
        let provider = DatabaseProvider::Memory;
        let debug_str = format!("{:?}", provider);
        assert!(debug_str.contains("Memory"));
    }
    #[test]
    fn test_database_provider_debug_format_sqlite() {
        let provider = DatabaseProvider::SQLite {
            path: "/test/path".to_string(),
        };
        let debug_str = format!("{:?}", provider);
        assert!(debug_str.contains("SQLite"));
        assert!(debug_str.contains("/test/path"));
    }
    #[test]
    fn test_database_provider_debug_format_postgres() {
        let provider = DatabaseProvider::Postgres {
            url: "postgres://localhost".to_string(),
        };
        let debug_str = format!("{:?}", provider);
        assert!(debug_str.contains("Postgres"));
        assert!(debug_str.contains("postgres://localhost"));
    }
    #[test]
    fn test_database_provider_clone() {
        let provider = DatabaseProvider::SQLite {
            path: "test.db".to_string(),
        };
        let cloned = provider.clone();
        matches::assert_matches!(cloned, DatabaseProvider::SQLite { path } if path == "test.db");
    }
    #[test]
    fn test_conversation_summary_construction_and_fields() {
        let summary = ConversationSummary {
            id: "conv-123".to_string(),
            title: "Test Conversation".to_string(),
            created_at: "2024-01-01".to_string(),
            updated_at: "2024-01-02".to_string(),
            message_count: 42,
        };
        assert_eq!(summary.id, "conv-123");
        assert_eq!(summary.title, "Test Conversation");
        assert_eq!(summary.created_at, "2024-01-01");
        assert_eq!(summary.updated_at, "2024-01-02");
        assert_eq!(summary.message_count, 42);
    }
    #[test]
    fn test_conversation_summary_debug_trait() {
        let summary = ConversationSummary {
            id: "test-id".to_string(),
            title: "Test".to_string(),
            created_at: "2024-01-01".to_string(),
            updated_at: "2024-01-02".to_string(),
            message_count: 1,
        };
        let debug_str = format!("{:?}", summary);
        assert!(debug_str.contains("ConversationSummary"));
    }
    #[test]
    fn test_conversation_summary_clone() {
        let summary = ConversationSummary {
            id: "original".to_string(),
            title: "Original Title".to_string(),
            created_at: "2024-01-01".to_string(),
            updated_at: "2024-01-02".to_string(),
            message_count: 10,
        };
        let cloned = summary.clone();
        assert_eq!(cloned.id, summary.id);
        assert_eq!(cloned.title, summary.title);
        assert_eq!(cloned.created_at, summary.created_at);
        assert_eq!(cloned.updated_at, summary.updated_at);
        assert_eq!(cloned.message_count, summary.message_count);
    }
    #[test]
    fn test_from_env_returns_memory_when_no_env_vars_set() {
        std::env::remove_var("DATABASE_URL");
        std::env::remove_var("DATABASE_PATH");
        std::env::remove_var("TURSO_URL");
        std::env::remove_var("TURSO_AUTH_TOKEN");
        let provider = DatabaseProvider::from_env();
        matches::assert_matches!(provider, DatabaseProvider::Memory);
    }

    #[test]
    fn test_from_env_returns_postgres_when_database_url_set() {
        std::env::remove_var("TURSO_URL");
        std::env::remove_var("DATABASE_PATH");
        std::env::set_var("DATABASE_URL", "postgres://example.com/ares");
        let provider = DatabaseProvider::from_env();
        matches::assert_matches!(
            provider,
            DatabaseProvider::Postgres { url } if url == "postgres://example.com/ares"
        );
        std::env::remove_var("DATABASE_URL");
    }

    #[test]
    fn test_from_env_ignores_empty_database_url() {
        std::env::remove_var("TURSO_URL");
        std::env::remove_var("DATABASE_PATH");
        std::env::set_var("DATABASE_URL", "   ");
        let provider = DatabaseProvider::from_env();
        matches::assert_matches!(provider, DatabaseProvider::Memory);
        std::env::remove_var("DATABASE_URL");
    }

    #[test]
    fn test_from_env_returns_sqlite_when_database_path_set() {
        std::env::remove_var("TURSO_URL");
        std::env::remove_var("DATABASE_URL");
        std::env::set_var("DATABASE_PATH", "/var/lib/ares/data.db");
        let provider = DatabaseProvider::from_env();
        matches::assert_matches!(
            provider,
            DatabaseProvider::SQLite { path } if path == "/var/lib/ares/data.db"
        );
        std::env::remove_var("DATABASE_PATH");
    }

    #[test]
    fn test_from_env_ignores_memory_database_path() {
        std::env::remove_var("TURSO_URL");
        std::env::remove_var("DATABASE_URL");
        std::env::set_var("DATABASE_PATH", ":memory:");
        let provider = DatabaseProvider::from_env();
        matches::assert_matches!(provider, DatabaseProvider::Memory);
        std::env::remove_var("DATABASE_PATH");
    }

    #[tokio::test]
    async fn create_client_postgres_rejects_unreachable_host() {
        let provider = DatabaseProvider::Postgres {
            url: "postgres://invalid:invalid@127.0.0.1:1/nope".into(),
        };
        assert!(provider.create_client().await.is_err());
    }

    #[tokio::test]
    async fn create_client_memory_executes_provider_branch() {
        std::env::set_var(
            "DATABASE_URL",
            "postgres://invalid:invalid@127.0.0.1:1/nope",
        );
        let provider = DatabaseProvider::Memory;
        let _ = provider.create_client().await;
        std::env::remove_var("DATABASE_URL");
    }

    #[tokio::test]
    async fn create_client_sqlite_executes_provider_branch() {
        std::env::set_var(
            "DATABASE_URL",
            "postgres://invalid:invalid@127.0.0.1:1/nope",
        );
        let provider = DatabaseProvider::SQLite {
            path: "/tmp/test.db".into(),
        };
        let _ = provider.create_client().await;
        std::env::remove_var("DATABASE_URL");
    }

    #[tokio::test]
    async fn database_client_stub_agent_methods_do_not_hit_database() {
        let client = PostgresClient::new_test();
        assert!(client.list_user_agents("user-1").await.unwrap().is_empty());
        assert!(client.list_public_agents(10, 0).await.unwrap().is_empty());
        let agent = UserAgent::new(
            "agent-1".into(),
            "user-1".into(),
            "coder".into(),
            "gpt-4".into(),
        );
        client.create_user_agent(&agent).await.unwrap();
        client.update_user_agent(&agent).await.unwrap();
        assert!(client.delete_user_agent("agent-1", "user-1").await.unwrap());
    }

    #[tokio::test]
    async fn database_client_create_user_maps_connection_error() {
        let client = unreachable_client();
        assert_database_error(
            client
                .create_user("u1", "a@b.com", "hash", "Alice")
                .await,
        );
    }

    #[tokio::test]
    async fn database_client_get_user_by_email_maps_connection_error() {
        let client = unreachable_client();
        assert_database_error(client.get_user_by_email("a@b.com").await);
    }

    #[tokio::test]
    async fn database_client_get_user_by_id_maps_connection_error() {
        let client = unreachable_client();
        assert_database_error(client.get_user_by_id("u1").await);
    }

    #[tokio::test]
    async fn database_client_session_methods_map_connection_error() {
        let client = unreachable_client();
        assert_database_error(
            client
                .create_session("s1", "u1", "token-hash", 9_999_999_999)
                .await,
        );
        assert_database_error(client.validate_session("token-hash").await);
        assert_database_error(client.delete_session("s1").await);
        assert_database_error(client.delete_session_by_token_hash("token-hash").await);
    }

    #[tokio::test]
    async fn database_client_conversation_methods_map_connection_error() {
        let client = unreachable_client();
        assert_database_error(
            client
                .create_conversation("c1", "u1", Some("title"))
                .await,
        );
        assert_database_error(client.conversation_exists("c1").await);
        assert_database_error(client.get_user_conversations("u1").await);
        assert_database_error(client.get_conversation("c1").await);
        assert_database_error(client.delete_conversation("c1").await);
        assert_database_error(
            client
                .update_conversation_title("c1", Some("new"))
                .await,
        );
    }

    #[tokio::test]
    async fn database_client_message_methods_map_connection_error() {
        let client = unreachable_client();
        assert_database_error(
            client
                .add_message("m1", "c1", MessageRole::User, "hello")
                .await,
        );
        assert_database_error(client.get_conversation_history("c1").await);
    }

    #[tokio::test]
    async fn database_client_memory_methods_map_connection_error() {
        let client = unreachable_client();
        let now = chrono::Utc::now();
        let fact = MemoryFact {
            id: "f1".into(),
            user_id: "u1".into(),
            category: "work".into(),
            fact_key: "role".into(),
            fact_value: "engineer".into(),
            confidence: 1.0,
            created_at: now,
            updated_at: now,
        };
        assert_database_error(client.store_memory_fact(&fact).await);
        assert_database_error(client.get_user_memory("u1").await);
        assert_database_error(client.get_memory_by_category("u1", "work").await);
    }

    #[tokio::test]
    async fn database_client_preference_methods_map_connection_error() {
        let client = unreachable_client();
        let preference = Preference {
            category: "ui".into(),
            key: "theme".into(),
            value: "dark".into(),
            confidence: 1.0,
        };
        assert_database_error(client.store_preference("u1", &preference).await);
        assert_database_error(client.get_user_preferences("u1").await);
        assert_database_error(client.get_preference("u1", "ui", "theme").await);
    }

    #[tokio::test]
    async fn database_client_agent_lookup_methods_map_connection_error() {
        let client = unreachable_client();
        assert_database_error(client.get_user_agent_by_name("u1", "coder").await);
        assert_database_error(client.get_public_agent_by_name("coder").await);
    }

}
