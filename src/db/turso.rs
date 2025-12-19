use crate::types::{AppError, MemoryFact, Message, MessageRole, Preference, Result};
use chrono::Utc;
use libsql::{params, Builder, Connection, Database};
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
              VALUES (?, ?, ?, ?, ?, ?)",
            (id, email, password_hash, name, now, now),
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
             VALUES (?, ?, ?, ?, ?)",
            (id, user_id, token_hash, expires_at, now),
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

        conn.execute(
            "INSERT INTO conversations (id, user_id, title, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?)",
            (id, user_id, title, now, now),
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create conversation: {}", e)))?;

        Ok(())
    }

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
             VALUES (?, ?, ?, ?, ?)",
            (id, conversation_id, role_str, content, now),
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

/// User-created agent stored in the database
/// This structure mirrors the TOON AgentConfig format for easy import/export
#[derive(Debug, Clone)]
pub struct UserAgent {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub model: String,
    pub system_prompt: Option<String>,
    /// JSON array of tool names: ["calculator", "web_search"]
    pub tools: String,
    pub max_tool_iterations: i32,
    pub parallel_tools: bool,
    /// JSON object for additional configuration
    pub extra: String,
    pub is_public: bool,
    pub usage_count: i32,
    pub rating_sum: i32,
    pub rating_count: i32,
    pub created_at: i64,
    pub updated_at: i64,
}

impl UserAgent {
    /// Create a new UserAgent with required fields
    pub fn new(id: String, user_id: String, name: String, model: String) -> Self {
        let now = Utc::now().timestamp();
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

    /// Get tools as a Vec<String>
    pub fn tools_vec(&self) -> Vec<String> {
        serde_json::from_str(&self.tools).unwrap_or_default()
    }

    /// Set tools from a Vec<String>
    pub fn set_tools(&mut self, tools: Vec<String>) {
        self.tools = serde_json::to_string(&tools).unwrap_or_else(|_| "[]".to_string());
    }

    /// Calculate average rating (returns None if no ratings)
    pub fn average_rating(&self) -> Option<f32> {
        if self.rating_count > 0 {
            Some(self.rating_sum as f32 / self.rating_count as f32)
        } else {
            None
        }
    }
}

/// Agent execution log entry for analytics
#[derive(Debug, Clone)]
pub struct AgentExecution {
    pub id: String,
    /// ID of user agent (None if system agent)
    pub agent_id: Option<String>,
    /// Name of the agent (always populated)
    pub agent_name: String,
    pub user_id: String,
    pub input: String,
    pub output: Option<String>,
    /// JSON array of tool invocations
    pub tool_calls: Option<String>,
    pub tokens_input: Option<i32>,
    pub tokens_output: Option<i32>,
    pub duration_ms: Option<i32>,
    /// Status: "success", "error", "timeout"
    pub status: String,
    pub error_message: Option<String>,
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
