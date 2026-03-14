use crate::types::{AppError, MemoryFact, Message, MessageRole, Preference, Result};
use chrono::{DateTime, Utc};
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;

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
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .map_err(|e| AppError::Database(format!("Failed to connect to Postgres: {}", e)))?;
        let client = Self { pool };
        Ok(client)
    }

    pub async fn new_local(_path: &str) -> Result<Self> {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/ares".to_string());
        Self::new_remote(url, "".to_string()).await
    }

    pub async fn new_memory() -> Result<Self> {
        Self::new_local("").await
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
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT user_id FROM sessions WHERE token_hash = $1 AND expires_at > $2",
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| AppError::Database(format!("Failed to validate session: {}", e)))?;
        Ok(row.map(|(id,)| id))
    }

    pub async fn delete_session(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Database(format!("Failed to delete session: {}", e)))?;
        Ok(())
    }

    pub async fn delete_session_by_token_hash(&self, token_hash: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
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
    ) -> Result<Vec<crate::db::traits::ConversationSummary>> {
        let rows = sqlx::query_as::<_, crate::db::traits::ConversationSummary>(
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
        let role_str = match role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
        };
        sqlx::query("INSERT INTO messages (id, conversation_id, role, content, timestamp) VALUES ($1, $2, $3, $4, $5)")
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
        let rows = sqlx::query_as::<_, MessageRow>("SELECT role, content, timestamp FROM messages WHERE conversation_id = $1 ORDER BY timestamp ASC")
            .bind(conversation_id).fetch_all(&self.pool).await.map_err(|e| AppError::Database(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| Message {
                role: match row.role.as_str() {
                    "system" => MessageRole::System,
                    "assistant" => MessageRole::Assistant,
                    _ => MessageRole::User,
                },
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
    pub fn tools_vec(&self) -> Vec<String> {
        serde_json::from_str(&self.tools).unwrap_or_default()
    }
    pub fn average_rating(&self) -> Option<f32> {
        if self.rating_count > 0 {
            Some(self.rating_sum as f32 / self.rating_count as f32)
        } else {
            None
        }
    }
}
