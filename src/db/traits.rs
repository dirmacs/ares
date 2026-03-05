use crate::types::{AppError, MemoryFact, Message, MessageRole, Preference, Result};
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
                let client = super::postgres::PostgresClient::new_remote(url.clone(), "".to_string()).await?;
                Ok(Box::new(client))
            }
        }
    }

    pub fn from_env() -> Self {
        if let Ok(url) = std::env::var("DATABASE_URL") {
            if !url.is_empty() { return DatabaseProvider::Postgres { url }; }
        }
        if let Ok(path) = std::env::var("DATABASE_PATH") {
            if !path.is_empty() && path != ":memory:" { return DatabaseProvider::SQLite { path }; }
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
    async fn create_user(&self, id: &str, email: &str, password_hash: &str, name: &str) -> Result<()>;
    async fn get_user_by_email(&self, email: &str) -> Result<Option<User>>;
    async fn get_user_by_id(&self, id: &str) -> Result<Option<User>>;
    async fn create_session(&self, id: &str, user_id: &str, token_hash: &str, expires_at: i64) -> Result<()>;
    async fn validate_session(&self, token_hash: &str) -> Result<Option<String>>;
    async fn delete_session(&self, id: &str) -> Result<()>;
    async fn delete_session_by_token_hash(&self, token_hash: &str) -> Result<()>;
    async fn create_conversation(&self, id: &str, user_id: &str, title: Option<&str>) -> Result<()>;
    async fn conversation_exists(&self, conversation_id: &str) -> Result<bool>;
    async fn get_user_conversations(&self, user_id: &str) -> Result<Vec<ConversationSummary>>;
    async fn get_conversation(&self, conversation_id: &str) -> Result<super::postgres::Conversation>;
    async fn delete_conversation(&self, conversation_id: &str) -> Result<()>;
    async fn update_conversation_title(&self, conversation_id: &str, title: Option<&str>) -> Result<()>;
    async fn add_message(&self, id: &str, conversation_id: &str, role: MessageRole, content: &str) -> Result<()>;
    async fn get_conversation_history(&self, conversation_id: &str) -> Result<Vec<Message>>;
    async fn store_memory_fact(&self, fact: &MemoryFact) -> Result<()>;
    async fn get_user_memory(&self, user_id: &str) -> Result<Vec<MemoryFact>>;
    async fn get_memory_by_category(&self, user_id: &str, category: &str) -> Result<Vec<MemoryFact>>;
    async fn store_preference(&self, user_id: &str, preference: &Preference) -> Result<()>;
    async fn get_user_preferences(&self, user_id: &str) -> Result<Vec<Preference>>;
    async fn get_preference(&self, user_id: &str, category: &str, key: &str) -> Result<Option<Preference>>;
    async fn get_user_agent_by_name(&self, user_id: &str, name: &str) -> Result<Option<super::postgres::UserAgent>>;
    async fn get_public_agent_by_name(&self, name: &str) -> Result<Option<super::postgres::UserAgent>>;
    async fn list_user_agents(&self, user_id: &str) -> Result<Vec<super::postgres::UserAgent>>;
    async fn list_public_agents(&self, limit: u32, offset: u32) -> Result<Vec<super::postgres::UserAgent>>;
    async fn create_user_agent(&self, agent: &super::postgres::UserAgent) -> Result<()>;
    async fn update_user_agent(&self, agent: &super::postgres::UserAgent) -> Result<()>;
    async fn delete_user_agent(&self, id: &str, user_id: &str) -> Result<bool>;
}

#[async_trait]
impl DatabaseClient for super::postgres::PostgresClient {
    async fn create_user(&self, id: &str, email: &str, password_hash: &str, name: &str) -> Result<()> { super::postgres::PostgresClient::create_user(self, id, email, password_hash, name).await }
    async fn get_user_by_email(&self, email: &str) -> Result<Option<User>> { super::postgres::PostgresClient::get_user_by_email(self, email).await }
    async fn get_user_by_id(&self, id: &str) -> Result<Option<User>> { super::postgres::PostgresClient::get_user_by_id(self, id).await }
    async fn create_session(&self, id: &str, user_id: &str, token_hash: &str, expires_at: i64) -> Result<()> { super::postgres::PostgresClient::create_session(self, id, user_id, token_hash, expires_at).await }
    async fn validate_session(&self, token_hash: &str) -> Result<Option<String>> { super::postgres::PostgresClient::validate_session(self, token_hash).await }
    async fn delete_session(&self, id: &str) -> Result<()> { super::postgres::PostgresClient::delete_session(self, id).await }
    async fn delete_session_by_token_hash(&self, token_hash: &str) -> Result<()> { super::postgres::PostgresClient::delete_session_by_token_hash(self, token_hash).await }
    async fn create_conversation(&self, id: &str, user_id: &str, title: Option<&str>) -> Result<()> { super::postgres::PostgresClient::create_conversation(self, id, user_id, title).await }
    async fn conversation_exists(&self, conversation_id: &str) -> Result<bool> { super::postgres::PostgresClient::conversation_exists(self, conversation_id).await }
    async fn get_user_conversations(&self, user_id: &str) -> Result<Vec<ConversationSummary>> { super::postgres::PostgresClient::get_user_conversations(self, user_id).await }
    async fn get_conversation(&self, conversation_id: &str) -> Result<super::postgres::Conversation> { 
        let row = sqlx::query_as::<_, super::postgres::Conversation>("SELECT id, user_id, title, created_at, updated_at, 0 as message_count FROM conversations WHERE id = $1").bind(conversation_id).fetch_optional(&self.pool).await.map_err(|e| AppError::Database(e.to_string()))?;
        row.ok_or_else(|| AppError::NotFound("Conversation not found".into()))
    }
    async fn delete_conversation(&self, conversation_id: &str) -> Result<()> { 
        sqlx::query("DELETE FROM messages WHERE conversation_id = $1").bind(conversation_id).execute(&self.pool).await.map_err(|e| AppError::Database(e.to_string()))?;
        sqlx::query("DELETE FROM conversations WHERE id = $1").bind(conversation_id).execute(&self.pool).await.map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }
    async fn update_conversation_title(&self, conversation_id: &str, title: Option<&str>) -> Result<()> { 
        let now = chrono::Utc::now().timestamp();
        sqlx::query("UPDATE conversations SET title = $1, updated_at = $2 WHERE id = $3").bind(title).bind(now).bind(conversation_id).execute(&self.pool).await.map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }
    async fn add_message(&self, id: &str, conversation_id: &str, role: MessageRole, content: &str) -> Result<()> { super::postgres::PostgresClient::add_message(self, id, conversation_id, role, content).await }
    async fn get_conversation_history(&self, conversation_id: &str) -> Result<Vec<Message>> { super::postgres::PostgresClient::get_conversation_history(self, conversation_id).await }
    async fn store_memory_fact(&self, fact: &MemoryFact) -> Result<()> { super::postgres::PostgresClient::store_memory_fact(self, fact).await }
    async fn get_user_memory(&self, user_id: &str) -> Result<Vec<MemoryFact>> { super::postgres::PostgresClient::get_user_memory(self, user_id).await }
    async fn get_memory_by_category(&self, user_id: &str, category: &str) -> Result<Vec<MemoryFact>> {
        let mems = super::postgres::PostgresClient::get_user_memory(self, user_id).await?;
        Ok(mems.into_iter().filter(|m| m.category == category).collect())
    }
    async fn store_preference(&self, user_id: &str, preference: &Preference) -> Result<()> { super::postgres::PostgresClient::store_preference(self, user_id, preference).await }
    async fn get_user_preferences(&self, user_id: &str) -> Result<Vec<Preference>> { super::postgres::PostgresClient::get_user_preferences(self, user_id).await }
    async fn get_preference(&self, user_id: &str, category: &str, key: &str) -> Result<Option<Preference>> {
        let prefs = super::postgres::PostgresClient::get_user_preferences(self, user_id).await?;
        Ok(prefs.into_iter().find(|p| p.category == category && p.key == key))
    }
    async fn get_user_agent_by_name(&self, user_id: &str, name: &str) -> Result<Option<super::postgres::UserAgent>> { super::postgres::PostgresClient::get_user_agent_by_name(self, user_id, name).await }
    async fn get_public_agent_by_name(&self, name: &str) -> Result<Option<super::postgres::UserAgent>> { 
        super::postgres::PostgresClient::get_user_agent_by_name(self, "", name).await 
    }
    async fn list_user_agents(&self, _user_id: &str) -> Result<Vec<super::postgres::UserAgent>> { Ok(vec![]) } 
    async fn list_public_agents(&self, _limit: u32, _offset: u32) -> Result<Vec<super::postgres::UserAgent>> { Ok(vec![]) } 
    async fn create_user_agent(&self, _agent: &super::postgres::UserAgent) -> Result<()> { Ok(()) } 
    async fn update_user_agent(&self, _agent: &super::postgres::UserAgent) -> Result<()> { Ok(()) } 
    async fn delete_user_agent(&self, _id: &str, _user_id: &str) -> Result<bool> { Ok(true) } 
}
