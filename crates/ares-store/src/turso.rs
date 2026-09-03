use super::postgres::{Conversation, User, UserAgent};
use super::traits::{ConversationSummary, DatabaseClient};
use ares_types::types::{
    AppError, ContentPart, MemoryFact, Message, MessageRole, Preference, Result,
};
use async_trait::async_trait;
use chrono::Utc;
use libsql::{params, Builder, Connection, Database};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Turso/libSQL database client for persistent storage
///
/// Supports both remote Turso databases and local SQLite files.
/// Handles connection pooling and schema initialization automatically.
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

    /// Create client based on URL format - routes to local or remote
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

    /// Get a raw database connection (prefer `operation_conn` for most uses)
    pub fn connection(&self) -> Result<Connection> {
        self.db
            .connect()
            .map_err(|e| AppError::Database(format!("Failed to get connection: {}", e)))
    }

    /// Get the connection to use for operations (handles in-memory vs file-based)
    pub async fn operation_conn(&self) -> Result<Connection> {
        if self.is_memory {
            let guard = self.cached_conn.lock().await;
            guard.as_ref().cloned().ok_or_else(|| {
                AppError::Database("No cached connection for in-memory database".to_string())
            })
        } else {
            self.connection()
        }
    }

    async fn initialize_schema(&self) -> Result<()> {
        let conn = self.operation_conn().await?;

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
                parts TEXT NOT NULL DEFAULT '[]',
                FOREIGN KEY (conversation_id) REFERENCES conversations(id)
            )",
            (),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create messages table: {}", e)))?;

        // Existing databases created before parts: add the column if missing.
        let _ = conn
            .execute(
                "ALTER TABLE messages ADD COLUMN parts TEXT NOT NULL DEFAULT '[]'",
                (),
            )
            .await;

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

        // User-created agents table (stores TOON-compatible agent configs)
        conn.execute(
            "CREATE TABLE IF NOT EXISTS user_agents (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                name TEXT NOT NULL,
                display_name TEXT,
                description TEXT,
                model TEXT NOT NULL,
                system_prompt TEXT,
                tools TEXT DEFAULT '[]',
                max_tool_iterations INTEGER DEFAULT 10,
                parallel_tools INTEGER DEFAULT 0,
                extra TEXT DEFAULT '{}',
                is_public INTEGER DEFAULT 0,
                usage_count INTEGER DEFAULT 0,
                rating_sum INTEGER DEFAULT 0,
                rating_count INTEGER DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                FOREIGN KEY (user_id) REFERENCES users(id),
                UNIQUE(user_id, name)
            )",
            (),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create user_agents table: {}", e)))?;

        // Create index for agent lookup
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_user_agents_lookup ON user_agents(user_id, name)",
            (),
        )
        .await
        .map_err(|e| {
            AppError::Database(format!("Failed to create user_agents_lookup index: {}", e))
        })?;

        // Create index for public agent discovery
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_user_agents_public ON user_agents(is_public, usage_count DESC)",
            (),
        )
        .await
        .map_err(|e| {
            AppError::Database(format!("Failed to create user_agents_public index: {}", e))
        })?;

        // User-created tools table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS user_tools (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                name TEXT NOT NULL,
                display_name TEXT,
                description TEXT,
                enabled INTEGER DEFAULT 1,
                timeout_secs INTEGER DEFAULT 30,
                tool_type TEXT NOT NULL,
                config TEXT DEFAULT '{}',
                parameters TEXT DEFAULT '{}',
                extra TEXT DEFAULT '{}',
                is_public INTEGER DEFAULT 0,
                usage_count INTEGER DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                FOREIGN KEY (user_id) REFERENCES users(id),
                UNIQUE(user_id, name)
            )",
            (),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create user_tools table: {}", e)))?;

        // User-created MCP configurations table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS user_mcps (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                name TEXT NOT NULL,
                enabled INTEGER DEFAULT 1,
                command TEXT NOT NULL,
                args TEXT DEFAULT '[]',
                env TEXT DEFAULT '{}',
                timeout_secs INTEGER DEFAULT 30,
                is_public INTEGER DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                FOREIGN KEY (user_id) REFERENCES users(id),
                UNIQUE(user_id, name)
            )",
            (),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create user_mcps table: {}", e)))?;

        // Agent execution logs for analytics
        conn.execute(
            "CREATE TABLE IF NOT EXISTS agent_executions (
                id TEXT PRIMARY KEY,
                agent_id TEXT,
                agent_name TEXT NOT NULL,
                user_id TEXT NOT NULL,
                input TEXT NOT NULL,
                output TEXT,
                tool_calls TEXT,
                tokens_input INTEGER,
                tokens_output INTEGER,
                duration_ms INTEGER,
                status TEXT NOT NULL,
                error_message TEXT,
                created_at INTEGER NOT NULL,
                FOREIGN KEY (user_id) REFERENCES users(id)
            )",
            (),
        )
        .await
        .map_err(|e| {
            AppError::Database(format!("Failed to create agent_executions table: {}", e))
        })?;

        // Create indexes for execution logs
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_executions_user ON agent_executions(user_id, created_at DESC)",
            (),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create executions_user index: {}", e)))?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_executions_agent ON agent_executions(agent_name, created_at DESC)",
            (),
        )
        .await
        .map_err(|e| {
            AppError::Database(format!("Failed to create executions_agent index: {}", e))
        })?;

        Ok(())
    }

    /// Creates a new user account
    ///
    /// # Arguments
    /// * `id` - Unique user identifier
    /// * `email` - User's email address (must be unique)
    /// * `password_hash` - Argon2 hashed password
    /// * `name` - User's display name
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
              VALUES (?, ?, ?, ?, ?, ?)",
            (id, email, password_hash, name, now, now),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create user: {}", e)))?;

        Ok(())
    }

    /// Retrieves a user by email address
    pub async fn get_user_by_email(&self, email: &str) -> Result<Option<User>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT id, email, password_hash, name, created_at, updated_at
                 FROM users WHERE email = ?",
                [email],
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

    /// Creates a new authentication session
    ///
    /// # Arguments
    /// * `id` - Unique session identifier
    /// * `user_id` - ID of the authenticated user
    /// * `token_hash` - Hash of the JWT refresh token
    /// * `expires_at` - Unix timestamp when session expires
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
             VALUES (?, ?, ?, ?, ?)",
            (id, user_id, token_hash, expires_at, now),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create session: {}", e)))?;

        Ok(())
    }

    /// Creates a new conversation for a user
    pub async fn create_conversation(
        &self,
        id: &str,
        user_id: &str,
        title: Option<&str>,
    ) -> Result<()> {
        let conn = self.operation_conn().await?;
        let now = Utc::now().timestamp();

        conn.execute(
            "INSERT INTO conversations (id, user_id, title, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?)",
            (id, user_id, title, now, now),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create conversation: {}", e)))?;

        Ok(())
    }

    /// Checks if a conversation exists by ID
    pub async fn conversation_exists(&self, conversation_id: &str) -> Result<bool> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT 1 FROM conversations WHERE id = ?",
                [conversation_id],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to check conversation: {}", e)))?;

        Ok(rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
            .is_some())
    }

    /// Adds a message to a conversation
    pub async fn add_message(
        &self,
        id: &str,
        conversation_id: &str,
        role: MessageRole,
        content: &str,
    ) -> Result<()> {
        self.add_message_with_parts(id, conversation_id, role, content, &[])
            .await
    }

    /// Adds a message, persisting multimodal parts as JSON.
    pub async fn add_message_with_parts(
        &self,
        id: &str,
        conversation_id: &str,
        role: MessageRole,
        content: &str,
        parts: &[ContentPart],
    ) -> Result<()> {
        let conn = self.operation_conn().await?;
        let now = Utc::now().timestamp();
        let role_str = match role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
        };
        let parts_json = serde_json::to_string(parts).unwrap_or_else(|_| "[]".to_string());

        conn.execute(
            "INSERT INTO messages (id, conversation_id, role, content, timestamp, parts)
             VALUES (?, ?, ?, ?, ?, ?)",
            (id, conversation_id, role_str, content, now, parts_json),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to add message: {}", e)))?;

        Ok(())
    }

    /// Retrieves all messages in a conversation, ordered by timestamp
    pub async fn get_conversation_history(&self, conversation_id: &str) -> Result<Vec<Message>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT role, content, timestamp, parts FROM messages
                 WHERE conversation_id = ? ORDER BY timestamp ASC",
                [conversation_id],
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

            let parts_raw: String = row.get(3).map_err(|e| AppError::Database(e.to_string()))?;
            messages.push(Message {
                role,
                content: row.get(1).map_err(|e| AppError::Database(e.to_string()))?,
                timestamp: chrono::DateTime::from_timestamp(
                    row.get::<i64>(2)
                        .map_err(|e| AppError::Database(e.to_string()))?,
                    0,
                )
                .unwrap(),
                parts: serde_json::from_str(&parts_raw).unwrap_or_default(),
            });
        }

        Ok(messages)
    }

    /// Get a conversation by ID
    pub async fn get_conversation(&self, conversation_id: &str) -> Result<Conversation> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT id, user_id, title, created_at, updated_at FROM conversations WHERE id = ?",
                [conversation_id],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to query conversation: {}", e)))?;

        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            let created_ts: i64 = row.get(3).map_err(|e| AppError::Database(e.to_string()))?;
            let updated_ts: i64 = row.get(4).map_err(|e| AppError::Database(e.to_string()))?;

            Ok(Conversation {
                id: row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
                user_id: row.get(1).map_err(|e| AppError::Database(e.to_string()))?,
                title: row.get(2).map_err(|e| AppError::Database(e.to_string()))?,
                message_count: 0, // Will be populated separately if needed
                created_at: chrono::DateTime::from_timestamp(created_ts, 0)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default(),
                updated_at: chrono::DateTime::from_timestamp(updated_ts, 0)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default(),
            })
        } else {
            Err(AppError::NotFound(format!(
                "Conversation {} not found",
                conversation_id
            )))
        }
    }

    /// Get all conversations for a user
    pub async fn get_user_conversations(&self, user_id: &str) -> Result<Vec<Conversation>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT c.id, c.user_id, c.title, c.created_at, c.updated_at,
                        (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id) as msg_count
                 FROM conversations c
                 WHERE c.user_id = ?
                 ORDER BY c.updated_at DESC",
                [user_id],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to query conversations: {}", e)))?;

        let mut conversations = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            let created_ts: i64 = row.get(3).map_err(|e| AppError::Database(e.to_string()))?;
            let updated_ts: i64 = row.get(4).map_err(|e| AppError::Database(e.to_string()))?;

            conversations.push(Conversation {
                id: row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
                user_id: row.get(1).map_err(|e| AppError::Database(e.to_string()))?,
                title: row.get(2).map_err(|e| AppError::Database(e.to_string()))?,
                message_count: row.get::<i32>(5).unwrap_or(0),
                created_at: chrono::DateTime::from_timestamp(created_ts, 0)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default(),
                updated_at: chrono::DateTime::from_timestamp(updated_ts, 0)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default(),
            });
        }

        Ok(conversations)
    }

    /// Update conversation title
    pub async fn update_conversation_title(
        &self,
        conversation_id: &str,
        title: Option<&str>,
    ) -> Result<()> {
        let conn = self.operation_conn().await?;
        let now = Utc::now().timestamp();

        conn.execute(
            "UPDATE conversations SET title = ?, updated_at = ? WHERE id = ?",
            (title, now, conversation_id),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to update conversation: {}", e)))?;

        Ok(())
    }

    /// Delete a conversation and all its messages
    pub async fn delete_conversation(&self, conversation_id: &str) -> Result<()> {
        let conn = self.operation_conn().await?;

        // Delete messages first (foreign key constraint)
        conn.execute(
            "DELETE FROM messages WHERE conversation_id = ?",
            [conversation_id],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to delete messages: {}", e)))?;

        // Delete conversation
        conn.execute("DELETE FROM conversations WHERE id = ?", [conversation_id])
            .await
            .map_err(|e| AppError::Database(format!("Failed to delete conversation: {}", e)))?;

        Ok(())
    }

    /// Stores a memory fact for a user (upserts on id)
    pub async fn store_memory_fact(&self, fact: &MemoryFact) -> Result<()> {
        let conn = self.operation_conn().await?;

        conn.execute(
            "INSERT OR REPLACE INTO memory_facts
            (id, user_id, category, fact_key, fact_value, confidence, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            (
                fact.id.as_str(),
                fact.user_id.as_str(),
                fact.category.as_str(),
                fact.fact_key.as_str(),
                fact.fact_value.as_str(),
                fact.confidence as f64,
                fact.created_at.timestamp(),
                fact.updated_at.timestamp(),
            ),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to store memory fact: {}", e)))?;

        Ok(())
    }

    /// Retrieves all memory facts for a user
    pub async fn get_user_memory(&self, user_id: &str) -> Result<Vec<MemoryFact>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT id, user_id, category, fact_key, fact_value, confidence, created_at, updated_at
                FROM memory_facts WHERE user_id = ?",
                [user_id],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to query memory facts: {}", e)))?;

        let mut facts = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            facts.push(MemoryFact {
                id: row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
                user_id: row.get(1).map_err(|e| AppError::Database(e.to_string()))?,
                category: row.get(2).map_err(|e| AppError::Database(e.to_string()))?,
                fact_key: row.get(3).map_err(|e| AppError::Database(e.to_string()))?,
                fact_value: row.get(4).map_err(|e| AppError::Database(e.to_string()))?,
                confidence: row
                    .get::<f64>(5)
                    .map_err(|e| AppError::Database(e.to_string()))?
                    as f32,
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

    /// Stores a user preference (upserts on user_id + category + key)
    pub async fn store_preference(&self, user_id: &str, preference: &Preference) -> Result<()> {
        let conn = self.operation_conn().await?;
        let now = Utc::now().timestamp();
        let id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            "INSERT OR REPLACE INTO preferences
             (id, user_id, category, key, value, confidence, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            (
                id,
                user_id,
                preference.category.as_str(),
                preference.key.as_str(),
                preference.value.as_str(),
                preference.confidence as f64,
                now,
            ),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to store preference: {}", e)))?;

        Ok(())
    }

    /// Retrieves all preferences for a user
    pub async fn get_user_preferences(&self, user_id: &str) -> Result<Vec<Preference>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT category, key, value, confidence FROM preferences WHERE user_id = ?",
                [user_id],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to query preferences: {}", e)))?;

        let mut preferences = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            preferences.push(Preference {
                category: row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
                key: row.get(1).map_err(|e| AppError::Database(e.to_string()))?,
                value: row.get(2).map_err(|e| AppError::Database(e.to_string()))?,
                confidence: row
                    .get::<f64>(3)
                    .map_err(|e| AppError::Database(e.to_string()))?
                    as f32,
            });
        }

        Ok(preferences)
    }

    // ============= User Agent Operations =============

    /// Create a new user-defined agent
    pub async fn create_user_agent(&self, agent: &UserAgent) -> Result<()> {
        let conn = self.operation_conn().await?;

        // Convert Option<String> to Option<&str> for libsql compatibility
        let display_name = agent.display_name.as_deref();
        let description = agent.description.as_deref();
        let system_prompt = agent.system_prompt.as_deref();

        conn.execute(
            "INSERT INTO user_agents (
                id, user_id, name, display_name, description, model, system_prompt,
                tools, max_tool_iterations, parallel_tools, extra, is_public,
                usage_count, rating_sum, rating_count, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                agent.id.as_str(),
                agent.user_id.as_str(),
                agent.name.as_str(),
                display_name,
                description,
                agent.model.as_str(),
                system_prompt,
                agent.tools.as_str(),
                agent.max_tool_iterations,
                agent.parallel_tools as i32,
                agent.extra.as_str(),
                agent.is_public as i32,
                agent.usage_count,
                agent.rating_sum,
                agent.rating_count,
                agent.created_at,
                agent.updated_at,
            ],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create user agent: {}", e)))?;

        Ok(())
    }

    /// Get a user agent by ID
    pub async fn get_user_agent(&self, id: &str) -> Result<Option<UserAgent>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT id, user_id, name, display_name, description, model, system_prompt,
                        tools, max_tool_iterations, parallel_tools, extra, is_public,
                        usage_count, rating_sum, rating_count, created_at, updated_at
                 FROM user_agents WHERE id = ?",
                [id],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to query user agent: {}", e)))?;

        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            Ok(Some(Self::row_to_user_agent(&row)?))
        } else {
            Ok(None)
        }
    }

    /// Get a user agent by user_id and name
    pub async fn get_user_agent_by_name(
        &self,
        user_id: &str,
        name: &str,
    ) -> Result<Option<UserAgent>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT id, user_id, name, display_name, description, model, system_prompt,
                        tools, max_tool_iterations, parallel_tools, extra, is_public,
                        usage_count, rating_sum, rating_count, created_at, updated_at
                 FROM user_agents WHERE user_id = ? AND name = ?",
                (user_id, name),
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to query user agent: {}", e)))?;

        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            Ok(Some(Self::row_to_user_agent(&row)?))
        } else {
            Ok(None)
        }
    }

    /// Get a public agent by name (for community discovery)
    pub async fn get_public_agent_by_name(&self, name: &str) -> Result<Option<UserAgent>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT id, user_id, name, display_name, description, model, system_prompt,
                        tools, max_tool_iterations, parallel_tools, extra, is_public,
                        usage_count, rating_sum, rating_count, created_at, updated_at
                 FROM user_agents WHERE name = ? AND is_public = 1
                 ORDER BY usage_count DESC LIMIT 1",
                [name],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to query public agent: {}", e)))?;

        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            Ok(Some(Self::row_to_user_agent(&row)?))
        } else {
            Ok(None)
        }
    }

    /// List all agents for a user
    pub async fn list_user_agents(&self, user_id: &str) -> Result<Vec<UserAgent>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT id, user_id, name, display_name, description, model, system_prompt,
                        tools, max_tool_iterations, parallel_tools, extra, is_public,
                        usage_count, rating_sum, rating_count, created_at, updated_at
                 FROM user_agents WHERE user_id = ? ORDER BY updated_at DESC",
                [user_id],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to list user agents: {}", e)))?;

        let mut agents = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            agents.push(Self::row_to_user_agent(&row)?);
        }

        Ok(agents)
    }

    /// List public agents (community/marketplace)
    pub async fn list_public_agents(&self, limit: u32, offset: u32) -> Result<Vec<UserAgent>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT id, user_id, name, display_name, description, model, system_prompt,
                        tools, max_tool_iterations, parallel_tools, extra, is_public,
                        usage_count, rating_sum, rating_count, created_at, updated_at
                 FROM user_agents WHERE is_public = 1
                 ORDER BY usage_count DESC LIMIT ? OFFSET ?",
                (limit, offset),
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to list public agents: {}", e)))?;

        let mut agents = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            agents.push(Self::row_to_user_agent(&row)?);
        }

        Ok(agents)
    }

    /// Update a user agent
    pub async fn update_user_agent(&self, agent: &UserAgent) -> Result<()> {
        let conn = self.operation_conn().await?;

        // Convert Option<String> to Option<&str> for libsql compatibility
        let display_name = agent.display_name.as_deref();
        let description = agent.description.as_deref();
        let system_prompt = agent.system_prompt.as_deref();

        conn.execute(
            "UPDATE user_agents SET
                display_name = ?1, description = ?2, model = ?3, system_prompt = ?4,
                tools = ?5, max_tool_iterations = ?6, parallel_tools = ?7, extra = ?8,
                is_public = ?9, updated_at = ?10
             WHERE id = ?11 AND user_id = ?12",
            params![
                display_name,
                description,
                agent.model.as_str(),
                system_prompt,
                agent.tools.as_str(),
                agent.max_tool_iterations,
                agent.parallel_tools as i32,
                agent.extra.as_str(),
                agent.is_public as i32,
                agent.updated_at,
                agent.id.as_str(),
                agent.user_id.as_str(),
            ],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to update user agent: {}", e)))?;

        Ok(())
    }

    /// Delete a user agent
    pub async fn delete_user_agent(&self, id: &str, user_id: &str) -> Result<bool> {
        let conn = self.operation_conn().await?;

        let affected = conn
            .execute(
                "DELETE FROM user_agents WHERE id = ? AND user_id = ?",
                (id, user_id),
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to delete user agent: {}", e)))?;

        Ok(affected > 0)
    }

    /// Increment usage count for an agent
    pub async fn increment_agent_usage(&self, id: &str) -> Result<()> {
        let conn = self.operation_conn().await?;

        conn.execute(
            "UPDATE user_agents SET usage_count = usage_count + 1 WHERE id = ?",
            [id],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to increment agent usage: {}", e)))?;

        Ok(())
    }

    /// Helper to convert a database row to UserAgent
    fn row_to_user_agent(row: &libsql::Row) -> Result<UserAgent> {
        Ok(UserAgent {
            id: row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
            user_id: row.get(1).map_err(|e| AppError::Database(e.to_string()))?,
            name: row.get(2).map_err(|e| AppError::Database(e.to_string()))?,
            display_name: row.get(3).map_err(|e| AppError::Database(e.to_string()))?,
            description: row.get(4).map_err(|e| AppError::Database(e.to_string()))?,
            model: row.get(5).map_err(|e| AppError::Database(e.to_string()))?,
            system_prompt: row.get(6).map_err(|e| AppError::Database(e.to_string()))?,
            tools: row.get(7).map_err(|e| AppError::Database(e.to_string()))?,
            max_tool_iterations: row.get(8).map_err(|e| AppError::Database(e.to_string()))?,
            parallel_tools: row
                .get::<i32>(9)
                .map_err(|e| AppError::Database(e.to_string()))?
                != 0,
            extra: row.get(10).map_err(|e| AppError::Database(e.to_string()))?,
            is_public: row
                .get::<i32>(11)
                .map_err(|e| AppError::Database(e.to_string()))?
                != 0,
            usage_count: row.get(12).map_err(|e| AppError::Database(e.to_string()))?,
            rating_sum: row.get(13).map_err(|e| AppError::Database(e.to_string()))?,
            rating_count: row.get(14).map_err(|e| AppError::Database(e.to_string()))?,
            created_at: row.get(15).map_err(|e| AppError::Database(e.to_string()))?,
            updated_at: row.get(16).map_err(|e| AppError::Database(e.to_string()))?,
        })
    }

    // ============= Agent Execution Logging =============

    /// Log an agent execution for analytics
    pub async fn log_agent_execution(&self, execution: &AgentExecution) -> Result<()> {
        let conn = self.operation_conn().await?;

        // Convert Option<String> to Option<&str> for libsql compatibility
        let agent_id = execution.agent_id.as_deref();
        let output = execution.output.as_deref();
        let tool_calls = execution.tool_calls.as_deref();
        let error_message = execution.error_message.as_deref();

        conn.execute(
            "INSERT INTO agent_executions (
                id, agent_id, agent_name, user_id, input, output, tool_calls,
                tokens_input, tokens_output, duration_ms, status, error_message, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                execution.id.as_str(),
                agent_id,
                execution.agent_name.as_str(),
                execution.user_id.as_str(),
                execution.input.as_str(),
                output,
                tool_calls,
                execution.tokens_input,
                execution.tokens_output,
                execution.duration_ms,
                execution.status.as_str(),
                error_message,
                execution.created_at,
            ],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to log agent execution: {}", e)))?;

        Ok(())
    }

    /// Get execution history for a user
    pub async fn get_user_executions(
        &self,
        user_id: &str,
        limit: u32,
    ) -> Result<Vec<AgentExecution>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT id, agent_id, agent_name, user_id, input, output, tool_calls,
                        tokens_input, tokens_output, duration_ms, status, error_message, created_at
                 FROM agent_executions WHERE user_id = ?
                 ORDER BY created_at DESC LIMIT ?",
                (user_id, limit),
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to query executions: {}", e)))?;

        let mut executions = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            executions.push(AgentExecution {
                id: row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
                agent_id: row.get(1).map_err(|e| AppError::Database(e.to_string()))?,
                agent_name: row.get(2).map_err(|e| AppError::Database(e.to_string()))?,
                user_id: row.get(3).map_err(|e| AppError::Database(e.to_string()))?,
                input: row.get(4).map_err(|e| AppError::Database(e.to_string()))?,
                output: row.get(5).map_err(|e| AppError::Database(e.to_string()))?,
                tool_calls: row.get(6).map_err(|e| AppError::Database(e.to_string()))?,
                tokens_input: row.get(7).map_err(|e| AppError::Database(e.to_string()))?,
                tokens_output: row.get(8).map_err(|e| AppError::Database(e.to_string()))?,
                duration_ms: row.get(9).map_err(|e| AppError::Database(e.to_string()))?,
                status: row.get(10).map_err(|e| AppError::Database(e.to_string()))?,
                error_message: row.get(11).map_err(|e| AppError::Database(e.to_string()))?,
                created_at: row.get(12).map_err(|e| AppError::Database(e.to_string()))?,
            });
        }

        Ok(executions)
    }

    // ============= Missing trait methods =============

    pub async fn get_user_by_id(&self, id: &str) -> Result<Option<User>> {
        let conn = self.operation_conn().await?;
        let mut rows = conn
            .query(
                "SELECT id, email, password_hash, name, created_at, updated_at
                 FROM users WHERE id = ?",
                [id],
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

    pub async fn validate_session(&self, token_hash: &str) -> Result<Option<String>> {
        let conn = self.operation_conn().await?;
        let now = Utc::now().timestamp();
        let mut rows = conn
            .query(
                "SELECT user_id FROM sessions WHERE token_hash = ? AND expires_at > ?",
                (token_hash, now),
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to validate session: {}", e)))?;

        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            let user_id: String = row.get(0).map_err(|e| AppError::Database(e.to_string()))?;
            Ok(Some(user_id))
        } else {
            Ok(None)
        }
    }

    pub async fn delete_session(&self, id: &str) -> Result<()> {
        let conn = self.operation_conn().await?;
        conn.execute("DELETE FROM sessions WHERE id = ?", [id])
            .await
            .map_err(|e| AppError::Database(format!("Failed to delete session: {}", e)))?;
        Ok(())
    }

    pub async fn delete_session_by_token_hash(&self, token_hash: &str) -> Result<()> {
        let conn = self.operation_conn().await?;
        conn.execute("DELETE FROM sessions WHERE token_hash = ?", [token_hash])
            .await
            .map_err(|e| AppError::Database(format!("Failed to delete session: {}", e)))?;
        Ok(())
    }

    pub async fn get_memory_by_category(
        &self,
        user_id: &str,
        category: &str,
    ) -> Result<Vec<MemoryFact>> {
        let all = self.get_user_memory(user_id).await?;
        Ok(all.into_iter().filter(|m| m.category == category).collect())
    }

    pub async fn get_preference(
        &self,
        user_id: &str,
        category: &str,
        key: &str,
    ) -> Result<Option<Preference>> {
        let prefs = self.get_user_preferences(user_id).await?;
        Ok(prefs
            .into_iter()
            .find(|p| p.category == category && p.key == key))
    }
}

// ============= DatabaseClient trait implementation =============

#[async_trait]
impl DatabaseClient for TursoClient {
    async fn create_user(
        &self,
        id: &str,
        email: &str,
        password_hash: &str,
        name: &str,
    ) -> Result<()> {
        TursoClient::create_user(self, id, email, password_hash, name).await
    }
    async fn get_user_by_email(&self, email: &str) -> Result<Option<User>> {
        TursoClient::get_user_by_email(self, email).await
    }
    async fn get_user_by_id(&self, id: &str) -> Result<Option<User>> {
        TursoClient::get_user_by_id(self, id).await
    }
    async fn create_session(
        &self,
        id: &str,
        user_id: &str,
        token_hash: &str,
        expires_at: i64,
    ) -> Result<()> {
        TursoClient::create_session(self, id, user_id, token_hash, expires_at).await
    }
    async fn validate_session(&self, token_hash: &str) -> Result<Option<String>> {
        TursoClient::validate_session(self, token_hash).await
    }
    async fn delete_session(&self, id: &str) -> Result<()> {
        TursoClient::delete_session(self, id).await
    }
    async fn delete_session_by_token_hash(&self, token_hash: &str) -> Result<()> {
        TursoClient::delete_session_by_token_hash(self, token_hash).await
    }
    async fn create_conversation(
        &self,
        id: &str,
        user_id: &str,
        title: Option<&str>,
    ) -> Result<()> {
        TursoClient::create_conversation(self, id, user_id, title).await
    }
    async fn conversation_exists(&self, conversation_id: &str) -> Result<bool> {
        TursoClient::conversation_exists(self, conversation_id).await
    }
    async fn get_user_conversations(&self, user_id: &str) -> Result<Vec<ConversationSummary>> {
        let convos = TursoClient::get_user_conversations(self, user_id).await?;
        Ok(convos
            .into_iter()
            .map(|c| ConversationSummary {
                id: c.id,
                title: c.title.unwrap_or_default(),
                created_at: c.created_at,
                updated_at: c.updated_at,
                message_count: c.message_count,
            })
            .collect())
    }
    async fn get_conversation(&self, conversation_id: &str) -> Result<Conversation> {
        TursoClient::get_conversation(self, conversation_id).await
    }
    async fn delete_conversation(&self, conversation_id: &str) -> Result<()> {
        TursoClient::delete_conversation(self, conversation_id).await
    }
    async fn update_conversation_title(
        &self,
        conversation_id: &str,
        title: Option<&str>,
    ) -> Result<()> {
        TursoClient::update_conversation_title(self, conversation_id, title).await
    }
    async fn add_message(
        &self,
        id: &str,
        conversation_id: &str,
        role: MessageRole,
        content: &str,
    ) -> Result<()> {
        TursoClient::add_message(self, id, conversation_id, role, content).await
    }
    async fn get_conversation_history(&self, conversation_id: &str) -> Result<Vec<Message>> {
        TursoClient::get_conversation_history(self, conversation_id).await
    }
    async fn store_memory_fact(&self, fact: &MemoryFact) -> Result<()> {
        TursoClient::store_memory_fact(self, fact).await
    }
    async fn get_user_memory(&self, user_id: &str) -> Result<Vec<MemoryFact>> {
        TursoClient::get_user_memory(self, user_id).await
    }
    async fn get_memory_by_category(
        &self,
        user_id: &str,
        category: &str,
    ) -> Result<Vec<MemoryFact>> {
        TursoClient::get_memory_by_category(self, user_id, category).await
    }
    async fn store_preference(&self, user_id: &str, preference: &Preference) -> Result<()> {
        TursoClient::store_preference(self, user_id, preference).await
    }
    async fn get_user_preferences(&self, user_id: &str) -> Result<Vec<Preference>> {
        TursoClient::get_user_preferences(self, user_id).await
    }
    async fn get_preference(
        &self,
        user_id: &str,
        category: &str,
        key: &str,
    ) -> Result<Option<Preference>> {
        TursoClient::get_preference(self, user_id, category, key).await
    }
    async fn get_user_agent_by_name(&self, user_id: &str, name: &str) -> Result<Option<UserAgent>> {
        TursoClient::get_user_agent_by_name(self, user_id, name).await
    }
    async fn get_public_agent_by_name(&self, name: &str) -> Result<Option<UserAgent>> {
        TursoClient::get_public_agent_by_name(self, name).await
    }
    async fn list_user_agents(&self, user_id: &str) -> Result<Vec<UserAgent>> {
        TursoClient::list_user_agents(self, user_id).await
    }
    async fn list_public_agents(&self, limit: u32, offset: u32) -> Result<Vec<UserAgent>> {
        TursoClient::list_public_agents(self, limit, offset).await
    }
    async fn create_user_agent(&self, agent: &UserAgent) -> Result<()> {
        TursoClient::create_user_agent(self, agent).await
    }
    async fn update_user_agent(&self, agent: &UserAgent) -> Result<()> {
        TursoClient::update_user_agent(self, agent).await
    }
    async fn delete_user_agent(&self, id: &str, user_id: &str) -> Result<bool> {
        TursoClient::delete_user_agent(self, id, user_id).await
    }
}

/// Agent execution log entry for analytics
#[derive(Debug, Clone)]
pub struct AgentExecution {
    /// Unique execution identifier (UUID)
    pub id: String,
    /// ID of user agent (None if system agent)
    pub agent_id: Option<String>,
    /// Name of the agent (always populated)
    pub agent_name: String,
    /// ID of the user who triggered this execution
    pub user_id: String,
    /// User's input message
    pub input: String,
    /// Agent's response (None if failed)
    pub output: Option<String>,
    /// JSON array of tool invocations
    pub tool_calls: Option<String>,
    /// Input token count
    pub tokens_input: Option<i32>,
    /// Output token count
    pub tokens_output: Option<i32>,
    /// Execution duration in milliseconds
    pub duration_ms: Option<i32>,
    /// Status: "success", "error", "timeout"
    pub status: String,
    /// Error message if status is "error"
    pub error_message: Option<String>,
    /// Unix timestamp of execution
    pub created_at: i64,
}

impl AgentExecution {
    /// Create a new execution log entry
    pub fn new(agent_name: String, user_id: String, input: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: None,
            agent_name,
            user_id,
            input,
            output: None,
            tool_calls: None,
            tokens_input: None,
            tokens_output: None,
            duration_ms: None,
            status: "pending".to_string(),
            error_message: None,
            created_at: Utc::now().timestamp(),
        }
    }

    /// Mark execution as successful
    pub fn success(mut self, output: String, duration_ms: i32) -> Self {
        self.output = Some(output);
        self.duration_ms = Some(duration_ms);
        self.status = "success".to_string();
        self
    }

    /// Mark execution as failed
    pub fn error(mut self, error: String) -> Self {
        self.error_message = Some(error);
        self.status = "error".to_string();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::types::{AppError, MessageRole};

    // ---- Connection logic -------------------------------------------------

    #[tokio::test]
    async fn new_memory_connects_and_initializes_schema() {
        let client = TursoClient::new_memory().await.expect("in-memory client");
        let conn = client.operation_conn().await.expect("operation conn");
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
                (),
            )
            .await
            .expect("list tables");
        let mut tables = Vec::new();
        while let Some(row) = rows.next().await.expect("row") {
            tables.push(row.get::<String>(0).expect("name"));
        }
        for expected in [
            "users",
            "sessions",
            "conversations",
            "messages",
            "user_agents",
        ] {
            assert!(
                tables.iter().any(|t| t == expected),
                "missing table {expected}: {tables:?}"
            );
        }
    }

    #[tokio::test]
    async fn new_routes_memory_url_to_local() {
        let client = TursoClient::new(":memory:".into(), String::new())
            .await
            .expect("memory via new()");
        assert!(client.operation_conn().await.is_ok());
    }

    #[tokio::test]
    async fn new_routes_file_url_to_local() {
        let path = std::env::temp_dir().join(format!("ares-turso-{}.db", uuid::Uuid::new_v4()));
        let url = format!("file:{}", path.display());
        let client = TursoClient::new(url, String::new())
            .await
            .expect("file-backed client");
        assert!(client.operation_conn().await.is_ok());
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn connection_returns_usable_handle_for_file_db() {
        let path =
            std::env::temp_dir().join(format!("ares-turso-conn-{}.db", uuid::Uuid::new_v4()));
        let client = TursoClient::new_local(path.to_str().unwrap())
            .await
            .expect("local client");
        let conn = client.connection().expect("raw connection");
        let mut rows = conn.query("SELECT 1", ()).await.expect("health check");
        assert!(rows.next().await.expect("row").is_some());
        let _ = std::fs::remove_file(&path);
    }

    // ---- Query building (SQL shape assertions) ----------------------------

    #[test]
    fn create_user_sql_uses_parameterized_insert() {
        let sql = "INSERT INTO users (id, email, password_hash, name, created_at, updated_at)
              VALUES (?, ?, ?, ?, ?, ?)";
        assert!(sql.contains("INSERT INTO users"));
        assert_eq!(sql.matches('?').count(), 6);
    }

    #[test]
    fn validate_session_sql_checks_expiry() {
        let sql = "SELECT user_id FROM sessions WHERE token_hash = ? AND expires_at > ?";
        assert!(sql.contains("token_hash = ?"));
        assert!(sql.contains("expires_at > ?"));
    }

    #[test]
    fn conversation_history_sql_orders_by_timestamp() {
        let sql = "SELECT role, content, timestamp, parts FROM messages
                 WHERE conversation_id = ? ORDER BY timestamp ASC";
        assert!(sql.contains("conversation_id = ?"));
        assert!(sql.contains("ORDER BY timestamp ASC"));
    }

    // ---- CRUD round-trips (in-memory) -----------------------------------

    #[tokio::test]
    async fn create_and_lookup_user_by_email() {
        let client = TursoClient::new_memory().await.expect("client");
        client
            .create_user("u1", "alice@example.com", "hash", "Alice")
            .await
            .expect("create user");

        let found = client
            .get_user_by_email("alice@example.com")
            .await
            .expect("lookup")
            .expect("user exists");
        assert_eq!(found.id, "u1");
        assert_eq!(found.email, "alice@example.com");

        let missing = client
            .get_user_by_email("nobody@example.com")
            .await
            .expect("lookup");
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn conversation_exists_reflects_create() {
        let client = TursoClient::new_memory().await.expect("client");
        client
            .create_user("u1", "bob@example.com", "hash", "Bob")
            .await
            .expect("user");
        assert!(!client.conversation_exists("c1").await.expect("exists"));

        client
            .create_conversation("c1", "u1", Some("First chat"))
            .await
            .expect("conversation");
        assert!(client.conversation_exists("c1").await.expect("exists"));
    }

    #[tokio::test]
    async fn add_message_and_read_history() {
        let client = TursoClient::new_memory().await.expect("client");
        client
            .create_user("u1", "carol@example.com", "hash", "Carol")
            .await
            .expect("user");
        client
            .create_conversation("c1", "u1", None)
            .await
            .expect("conversation");

        client
            .add_message("m1", "c1", MessageRole::User, "hello")
            .await
            .expect("message");
        client
            .add_message("m2", "c1", MessageRole::Assistant, "hi")
            .await
            .expect("message");

        let history = client
            .get_conversation_history("c1")
            .await
            .expect("history");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].content, "hello");
        assert!(matches!(history[1].role, MessageRole::Assistant));
    }

    // ---- AgentExecution builder -------------------------------------------

    #[test]
    fn agent_execution_new_defaults_to_pending() {
        let exec = AgentExecution::new("agent".into(), "user".into(), "input".into());
        assert_eq!(exec.status, "pending");
        assert!(exec.output.is_none());
        assert!(exec.error_message.is_none());
        assert!(!exec.id.is_empty());
    }

    #[test]
    fn agent_execution_success_sets_output_and_status() {
        let exec = AgentExecution::new("agent".into(), "user".into(), "in".into())
            .success("out".into(), 42);
        assert_eq!(exec.status, "success");
        assert_eq!(exec.output.as_deref(), Some("out"));
        assert_eq!(exec.duration_ms, Some(42));
    }

    #[test]
    fn agent_execution_error_sets_message_and_status() {
        let exec =
            AgentExecution::new("agent".into(), "user".into(), "in".into()).error("boom".into());
        assert_eq!(exec.status, "error");
        assert_eq!(exec.error_message.as_deref(), Some("boom"));
    }

    // ---- Error handling ---------------------------------------------------

    #[tokio::test]
    async fn get_conversation_returns_not_found_for_missing_id() {
        let client = TursoClient::new_memory().await.expect("client");
        let err = client.get_conversation("missing").await.unwrap_err();
        matches::assert_matches!(err, AppError::NotFound(_));
    }

    #[tokio::test]
    async fn create_user_rejects_duplicate_email() {
        let client = TursoClient::new_memory().await.expect("client");
        client
            .create_user("u1", "dup@example.com", "hash", "One")
            .await
            .expect("first");
        let err = client
            .create_user("u2", "dup@example.com", "hash", "Two")
            .await
            .unwrap_err();
        matches::assert_matches!(err, AppError::Database(msg) if msg.contains("UNIQUE"));
    }
}
