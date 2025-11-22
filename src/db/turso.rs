use crate::types::{AppError, MemoryFact, Message, MessageRole, Preference, Result};
use chrono::Utc;
use libsql::{Builder, Connection, Database};

pub struct TursoClient {
    db: Database,
}

impl TursoClient {
    pub async fn new(url: String, auth_token: String) -> Result<Self> {
        let db = Builder::new_remote(url, auth_token)
            .build()
            .await
            .map_err(|e| AppError::Database(format!("Failed to connect to Turso: {}", e)))?;

        let client = Self { db };
        client.initialize_schema().await?;

        Ok(client)
    }

    pub fn connection(&self) -> Result<Connection> {
        self.db
            .connect()
            .map_err(|e| AppError::Database(format!("Failed to get connection: {}", e)))
    }

    async fn initialize_schema(&self) -> Result<()> {
        let conn = self.connection()?;

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

    // User operations
    pub async fn create_user(
        &self,
        id: &str,
        email: &str,
        password_hash: &str,
        name: &str,
    ) -> Result<()> {
        let conn = self.connection()?;
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
        let conn = self.connection()?;

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
        let conn = self.connection()?;
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
        let conn = self.connection()?;
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

    pub async fn add_message(
        &self,
        id: &str,
        conversation_id: &str,
        role: MessageRole,
        content: &str,
    ) -> Result<()> {
        let conn = self.connection()?;
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

    // pub async fn get_conversation_history(&self, conversation_id: &str) -> Result<Vec<Message>> {
    //     let conn = self.connection()?;
    // }
}
