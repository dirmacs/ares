use crate::types::{AppError, MemoryFact, Message, MessageRole, Preference, Result};
use chrono::Utc;
use libsql::{Builder, Connection, Database};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct TursoClient {
    db: Database,
    /// Cached connection for in-memory databases to ensure schema persists
    cached_conn: Arc<Mutex<Option<Connection>>>,
    is_memory: bool,
}

impl TursoClient {
    /// Create a new TursoClient with remote Turso database
    pub async fn new_remote(url: String, auth_token: String) -> Result<Self> {
        let db = Builder::new_remote(url, auth_token)
            .build()
            .await
            .map_err(|e| AppError::Database(format!("Failed to connect to Turso: {}", e)))?;

        let client = Self {
            db,
            cached_conn: Arc::new(Mutex::new(None)),
            is_memory: false,
        };
        client.initialize_schema().await?;

        Ok(client)
    }

    /// Create a new TursoClient with local SQLite database
    pub async fn new_local(path: &str) -> Result<Self> {
        let is_memory = path == ":memory:";
        let db = Builder::new_local(path)
            .build()
            .await
            .map_err(|e| AppError::Database(format!("Failed to open local database: {}", e)))?;

        let client = Self {
            db,
            cached_conn: Arc::new(Mutex::new(None)),
            is_memory,
        };

        // For in-memory databases, we need to cache the connection
        // so that schema persists across calls
        if is_memory {
            let conn = client
                .db
                .connect()
                .map_err(|e| AppError::Database(format!("Failed to get connection: {}", e)))?;
            *client.cached_conn.lock().await = Some(conn);
        }

        client.initialize_schema().await?;

        Ok(client)
    }

    /// Create a new TursoClient with in-memory database (useful for testing)
    pub async fn new_memory() -> Result<Self> {
        Self::new_local(":memory:").await
    }

    /// Create client based on environment - prefers local, falls back to remote if configured
    pub async fn new(url: String, auth_token: String) -> Result<Self> {
        // If URL starts with "file:" or is a path, use local mode
        if url.starts_with("file:") || url.ends_with(".db") || url == ":memory:" {
            Self::new_local(&url).await
        } else if url.starts_with("libsql://") || url.starts_with("https://") {
            Self::new_remote(url, auth_token).await
        } else {
            // Default to local with the URL as path
            Self::new_local(&url).await
        }
    }

    pub fn connection(&self) -> Result<Connection> {
        // For in-memory databases, we can't easily return the cached connection
        // because Connection doesn't implement Clone. Instead, we create new ones
        // for non-memory databases.
        self.db
            .connect()
            .map_err(|e| AppError::Database(format!("Failed to get connection: {}", e)))
    }

    async fn initialize_schema(&self) -> Result<()> {
        let conn = if self.is_memory {
            let guard = self.cached_conn.lock().await;
            guard.as_ref().cloned().ok_or_else(|| {
                AppError::Database("No cached connection for in-memory database".to_string())
            })?
        } else {
            self.connection()?
        };

        // Users table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS users (
                id TEXT PRIMARY KEY,
                email TEXT UNIQUE NOT NULL,
                password_hash TEXT NOT NULL,
                name TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
            (),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create users table: {}", e)))?;

        // Sessions table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                token_hash TEXT NOT NULL,
                expires_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                FOREIGN KEY (user_id) REFERENCES users(id)
            )",
            (),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create sessions table: {}", e)))?;

        // Conversations table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS conversations (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                title TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                FOREIGN KEY (user_id) REFERENCES users(id)
            )",
            (),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create conversations table: {}", e)))?;

        // Messages table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                FOREIGN KEY (conversation_id) REFERENCES conversations(id)
            )",
            (),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create messages table: {}", e)))?;

        // Memory facts table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS memory_facts (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                category TEXT NOT NULL,
                fact_key TEXT NOT NULL,
                fact_value TEXT NOT NULL,
                confidence REAL NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                FOREIGN KEY (user_id) REFERENCES users(id)
            )",
            (),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create memory_facts table: {}", e)))?;

        // Preferences table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS preferences (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                category TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                confidence REAL NOT NULL,
                created_at INTEGER NOT NULL,
                FOREIGN KEY (user_id) REFERENCES users(id),
                UNIQUE(user_id, category, key)
            )",
            (),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create preferences table: {}", e)))?;

        Ok(())
    }

    /// Get the connection to use for operations (handles in-memory vs file-based)
    async fn operation_conn(&self) -> Result<Connection> {
        if self.is_memory {
            let guard = self.cached_conn.lock().await;
            guard.as_ref().cloned().ok_or_else(|| {
                AppError::Database("No cached connection for in-memory database".to_string())
            })
        } else {
            self.connection()
        }
    }

    // User operations
    pub async fn create_user(
        &self,
        id: &str,
        email: &str,
        password_hash: &str,
        name: &str,
    ) -> Result<()> {
        let conn = self.operation_conn().await?;
        let now = Utc::now().timestamp();

        conn.execute(
            "INSERT INTO users (id, email, password_hash, name, created_at, updated_at)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            libsql::params![id, email, password_hash, name, now, now],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create user: {}", e)))?;

        Ok(())
    }

    pub async fn get_user_by_email(&self, email: &str) -> Result<Option<User>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT id, email, password_hash, name, created_at, updated_at
                 FROM users WHERE email = ?1",
                libsql::params![email],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to query user: {}", e)))?;

        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            Ok(Some(User {
                id: row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
                email: row.get(1).map_err(|e| AppError::Database(e.to_string()))?,
                password_hash: row.get(2).map_err(|e| AppError::Database(e.to_string()))?,
                name: row.get(3).map_err(|e| AppError::Database(e.to_string()))?,
                created_at: row.get(4).map_err(|e| AppError::Database(e.to_string()))?,
                updated_at: row.get(5).map_err(|e| AppError::Database(e.to_string()))?,
            }))
        } else {
            Ok(None)
        }
    }

    // Session operations
    pub async fn create_session(
        &self,
        id: &str,
        user_id: &str,
        token_hash: &str,
        expires_at: i64,
    ) -> Result<()> {
        let conn = self.operation_conn().await?;
        let now = Utc::now().timestamp();

        conn.execute(
            "INSERT INTO sessions (id, user_id, token_hash, expires_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            libsql::params![id, user_id, token_hash, expires_at, now],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create session: {}", e)))?;

        Ok(())
    }

    // Conversation operations
    pub async fn create_conversation(
        &self,
        id: &str,
        user_id: &str,
        title: Option<&str>,
    ) -> Result<()> {
        let conn = self.operation_conn().await?;
        let now = Utc::now().timestamp();
        let title_str = title.unwrap_or("");

        conn.execute(
            "INSERT INTO conversations (id, user_id, title, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            libsql::params![id, user_id, title_str, now, now],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create conversation: {}", e)))?;

        Ok(())
    }

    pub async fn add_message(
        &self,
        id: &str,
        conversation_id: &str,
        role: MessageRole,
        content: &str,
    ) -> Result<()> {
        let conn = self.operation_conn().await?;
        let now = Utc::now().timestamp();
        let role_str = match role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
        };

        conn.execute(
            "INSERT INTO messages (id, conversation_id, role, content, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            libsql::params![id, conversation_id, role_str, content, now],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to add message: {}", e)))?;

        Ok(())
    }

    pub async fn get_conversation_history(&self, conversation_id: &str) -> Result<Vec<Message>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT role, content, timestamp FROM messages
                 WHERE conversation_id = ?1 ORDER BY timestamp ASC",
                libsql::params![conversation_id],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to query messages: {}", e)))?;

        let mut messages = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            let role_str: String = row.get(0).map_err(|e| AppError::Database(e.to_string()))?;
            let role = match role_str.as_str() {
                "system" => MessageRole::System,
                "user" => MessageRole::User,
                "assistant" => MessageRole::Assistant,
                _ => MessageRole::User,
            };

            messages.push(Message {
                role,
                content: row.get(1).map_err(|e| AppError::Database(e.to_string()))?,
                timestamp: chrono::DateTime::from_timestamp(
                    row.get::<i64>(2)
                        .map_err(|e| AppError::Database(e.to_string()))?,
                    0,
                )
                .unwrap(),
            });
        }

        Ok(messages)
    }

    // Memory operations
    pub async fn store_memory_fact(&self, fact: &MemoryFact) -> Result<()> {
        let conn = self.operation_conn().await?;

        conn.execute(
            "INSERT OR REPLACE INTO memory_facts
            (id, user_id, category, fact_key, fact_value, confidence, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            libsql::params![
                fact.id.as_str(),
                fact.user_id.as_str(),
                fact.category.as_str(),
                fact.fact_key.as_str(),
                fact.fact_value.as_str(),
                fact.confidence as f64,
                fact.created_at.timestamp(),
                fact.updated_at.timestamp(),
            ],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to store memory fact: {}", e)))?;

        Ok(())
    }

    pub async fn get_user_memory(&self, user_id: &str) -> Result<Vec<MemoryFact>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT id, user_id, category, fact_key, fact_value, confidence, created_at, updated_at
                FROM memory_facts WHERE user_id = ?1",
                libsql::params![user_id],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to query memory facts: {}", e)))?;

        let mut facts = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            let confidence: f64 = row.get(5).map_err(|e| AppError::Database(e.to_string()))?;
            facts.push(MemoryFact {
                id: row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
                user_id: row.get(1).map_err(|e| AppError::Database(e.to_string()))?,
                category: row.get(2).map_err(|e| AppError::Database(e.to_string()))?,
                fact_key: row.get(3).map_err(|e| AppError::Database(e.to_string()))?,
                fact_value: row.get(4).map_err(|e| AppError::Database(e.to_string()))?,
                confidence: confidence as f32,
                created_at: chrono::DateTime::from_timestamp(
                    row.get::<i64>(6)
                        .map_err(|e| AppError::Database(e.to_string()))?,
                    0,
                )
                .unwrap(),
                updated_at: chrono::DateTime::from_timestamp(
                    row.get::<i64>(7)
                        .map_err(|e| AppError::Database(e.to_string()))?,
                    0,
                )
                .unwrap(),
            });
        }

        Ok(facts)
    }

    pub async fn store_preference(&self, user_id: &str, preference: &Preference) -> Result<()> {
        let conn = self.operation_conn().await?;
        let now = Utc::now().timestamp();
        let id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            "INSERT OR REPLACE INTO preferences
             (id, user_id, category, key, value, confidence, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            libsql::params![
                id,
                user_id,
                preference.category.as_str(),
                preference.key.as_str(),
                preference.value.as_str(),
                preference.confidence as f64,
                now,
            ],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to store preference: {}", e)))?;

        Ok(())
    }

    pub async fn get_user_preferences(&self, user_id: &str) -> Result<Vec<Preference>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT category, key, value, confidence FROM preferences WHERE user_id = ?1",
                libsql::params![user_id],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to query preferences: {}", e)))?;

        let mut preferences = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            let confidence: f64 = row.get(3).map_err(|e| AppError::Database(e.to_string()))?;
            preferences.push(Preference {
                category: row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
                key: row.get(1).map_err(|e| AppError::Database(e.to_string()))?,
                value: row.get(2).map_err(|e| AppError::Database(e.to_string()))?,
                confidence: confidence as f32,
            });
        }

        Ok(preferences)
    }
}

#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    pub email: String,
    pub password_hash: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
}
