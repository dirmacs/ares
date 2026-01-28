//! Database abstraction traits
//!
//! This module provides the `DatabaseClient` trait that abstracts over different
//! database backends (in-memory SQLite, file-based SQLite, remote Turso).
//!
//! # Example
//!
//! ```rust,ignore
//! use ares::db::{DatabaseClient, DatabaseProvider};
//!
//! // Use in-memory database (default for development/testing)
//! let db = DatabaseProvider::Memory.create_client().await?;
//!
//! // Use file-based SQLite
//! let db = DatabaseProvider::SQLite { path: "data.db".into() }.create_client().await?;
//!
//! // Use remote Turso (requires `turso` feature)
//! let db = DatabaseProvider::Turso { url, token }.create_client().await?;
//! ```

use crate::types::{AppError, MemoryFact, Message, MessageRole, Preference, Result};
use async_trait::async_trait;

/// Database provider configuration
#[derive(Debug, Clone, Default)]
pub enum DatabaseProvider {
    /// In-memory SQLite database (ephemeral, lost on restart)
    #[default]
    Memory,
    /// File-based SQLite database
    SQLite {
        /// Path to the SQLite database file
        path: String,
    },
    /// Remote Turso database (requires network access)
    #[cfg(feature = "turso")]
    Turso {
        /// The Turso database URL (e.g., `libsql://your-db.turso.io`)
        url: String,
        /// Authentication token for the Turso database
        auth_token: String,
    },
}

impl DatabaseProvider {
    /// Create a database client from this provider configuration
    pub async fn create_client(&self) -> Result<Box<dyn DatabaseClient>> {
        match self {
            DatabaseProvider::Memory => {
                let client = super::turso::TursoClient::new_memory().await?;
                Ok(Box::new(client))
            }
            DatabaseProvider::SQLite { path } => {
                let client = super::turso::TursoClient::new_local(path).await?;
                Ok(Box::new(client))
            }
            #[cfg(feature = "turso")]
            DatabaseProvider::Turso { url, auth_token } => {
                let client =
                    super::turso::TursoClient::new_remote(url.clone(), auth_token.clone()).await?;
                Ok(Box::new(client))
            }
        }
    }

    /// Create from environment variables or use defaults
    pub fn from_env() -> Self {
        // Check for Turso configuration first
        #[cfg(feature = "turso")]
        {
            if let (Ok(url), Ok(token)) = (
                std::env::var("TURSO_DATABASE_URL"),
                std::env::var("TURSO_AUTH_TOKEN"),
            ) {
                if !url.is_empty() && !token.is_empty() {
                    return DatabaseProvider::Turso {
                        url,
                        auth_token: token,
                    };
                }
            }
        }

        // Check for SQLite file path
        if let Ok(path) = std::env::var("DATABASE_PATH") {
            if !path.is_empty() && path != ":memory:" {
                return DatabaseProvider::SQLite { path };
            }
        }

        // Default to in-memory
        DatabaseProvider::Memory
    }
}

/// User record from the database
pub use super::turso::User;

/// Summary of a conversation (without full message history)
#[derive(Debug, Clone)]
pub struct ConversationSummary {
    /// Unique conversation identifier
    pub id: String,
    /// Conversation title
    pub title: String,
    /// Unix timestamp of creation
    pub created_at: i64,
    /// Unix timestamp of last update
    pub updated_at: i64,
    /// Number of messages in conversation
    pub message_count: i64,
}

/// Abstract trait for database operations
///
/// This trait defines all database operations needed by the application.
/// Implementations can use different backends (SQLite, Turso, etc.)
#[async_trait]
pub trait DatabaseClient: Send + Sync {
    // ============== User Operations ==============

    /// Create a new user
    async fn create_user(
        &self,
        id: &str,
        email: &str,
        password_hash: &str,
        name: &str,
    ) -> Result<()>;

    /// Get a user by email
    async fn get_user_by_email(&self, email: &str) -> Result<Option<User>>;

    /// Get a user by ID
    async fn get_user_by_id(&self, id: &str) -> Result<Option<User>>;

    // ============== Session Operations ==============

    /// Create a new session
    async fn create_session(
        &self,
        id: &str,
        user_id: &str,
        token_hash: &str,
        expires_at: i64,
    ) -> Result<()>;

    /// Validate and get session (returns user_id if valid)
    async fn validate_session(&self, token_hash: &str) -> Result<Option<String>>;

    /// Delete a session by ID
    async fn delete_session(&self, id: &str) -> Result<()>;

    /// Delete a session by token hash (for refresh token invalidation)
    async fn delete_session_by_token_hash(&self, token_hash: &str) -> Result<()>;

    // ============== Conversation Operations ==============

    /// Create a new conversation
    async fn create_conversation(&self, id: &str, user_id: &str, title: Option<&str>)
        -> Result<()>;

    /// Check if a conversation exists
    async fn conversation_exists(&self, conversation_id: &str) -> Result<bool>;

    /// Get conversations for a user
    async fn get_user_conversations(&self, user_id: &str) -> Result<Vec<ConversationSummary>>;

    /// Add a message to a conversation
    async fn add_message(
        &self,
        id: &str,
        conversation_id: &str,
        role: MessageRole,
        content: &str,
    ) -> Result<()>;

    /// Get conversation history
    async fn get_conversation_history(&self, conversation_id: &str) -> Result<Vec<Message>>;

    // ============== Memory Operations ==============

    /// Store a memory fact
    async fn store_memory_fact(&self, fact: &MemoryFact) -> Result<()>;

    /// Get all memory facts for a user
    async fn get_user_memory(&self, user_id: &str) -> Result<Vec<MemoryFact>>;

    /// Get memory facts by category
    async fn get_memory_by_category(
        &self,
        user_id: &str,
        category: &str,
    ) -> Result<Vec<MemoryFact>>;

    // ============== Preference Operations ==============

    /// Store a user preference
    async fn store_preference(&self, user_id: &str, preference: &Preference) -> Result<()>;

    /// Get all preferences for a user
    async fn get_user_preferences(&self, user_id: &str) -> Result<Vec<Preference>>;

    /// Get preference by category and key
    async fn get_preference(
        &self,
        user_id: &str,
        category: &str,
        key: &str,
    ) -> Result<Option<Preference>>;
}

// ============== Implement DatabaseClient for TursoClient ==============

#[async_trait]
impl DatabaseClient for super::turso::TursoClient {
    async fn create_user(
        &self,
        id: &str,
        email: &str,
        password_hash: &str,
        name: &str,
    ) -> Result<()> {
        super::turso::TursoClient::create_user(self, id, email, password_hash, name).await
    }

    async fn get_user_by_email(&self, email: &str) -> Result<Option<User>> {
        super::turso::TursoClient::get_user_by_email(self, email).await
    }

    async fn get_user_by_id(&self, id: &str) -> Result<Option<User>> {
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

    async fn create_session(
        &self,
        id: &str,
        user_id: &str,
        token_hash: &str,
        expires_at: i64,
    ) -> Result<()> {
        super::turso::TursoClient::create_session(self, id, user_id, token_hash, expires_at).await
    }

    async fn validate_session(&self, token_hash: &str) -> Result<Option<String>> {
        let conn = self.operation_conn().await?;
        let now = chrono::Utc::now().timestamp();

        let mut rows = conn
            .query(
                "SELECT user_id FROM sessions WHERE token_hash = ? AND expires_at > ?",
                [token_hash, &now.to_string()],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to validate session: {}", e)))?;

        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            Ok(Some(
                row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
            ))
        } else {
            Ok(None)
        }
    }

    async fn delete_session(&self, id: &str) -> Result<()> {
        let conn = self.operation_conn().await?;

        conn.execute("DELETE FROM sessions WHERE id = ?", [id])
            .await
            .map_err(|e| AppError::Database(format!("Failed to delete session: {}", e)))?;

        Ok(())
    }

    async fn delete_session_by_token_hash(&self, token_hash: &str) -> Result<()> {
        let conn = self.operation_conn().await?;

        conn.execute("DELETE FROM sessions WHERE token_hash = ?", [token_hash])
            .await
            .map_err(|e| {
                AppError::Database(format!("Failed to delete session by token hash: {}", e))
            })?;

        Ok(())
    }

    async fn create_conversation(
        &self,
        id: &str,
        user_id: &str,
        title: Option<&str>,
    ) -> Result<()> {
        super::turso::TursoClient::create_conversation(self, id, user_id, title).await
    }

    async fn conversation_exists(&self, conversation_id: &str) -> Result<bool> {
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

    async fn get_user_conversations(&self, user_id: &str) -> Result<Vec<ConversationSummary>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT c.id, c.title, c.created_at, c.updated_at,
                        (SELECT COUNT(*) FROM messages WHERE conversation_id = c.id) as msg_count
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
            conversations.push(ConversationSummary {
                id: row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
                title: row.get::<String>(1).unwrap_or_default(),
                created_at: row.get(2).map_err(|e| AppError::Database(e.to_string()))?,
                updated_at: row.get(3).map_err(|e| AppError::Database(e.to_string()))?,
                message_count: row.get(4).map_err(|e| AppError::Database(e.to_string()))?,
            });
        }

        Ok(conversations)
    }

    async fn add_message(
        &self,
        id: &str,
        conversation_id: &str,
        role: MessageRole,
        content: &str,
    ) -> Result<()> {
        super::turso::TursoClient::add_message(self, id, conversation_id, role, content).await
    }

    async fn get_conversation_history(&self, conversation_id: &str) -> Result<Vec<Message>> {
        super::turso::TursoClient::get_conversation_history(self, conversation_id).await
    }

    async fn store_memory_fact(&self, fact: &MemoryFact) -> Result<()> {
        super::turso::TursoClient::store_memory_fact(self, fact).await
    }

    async fn get_user_memory(&self, user_id: &str) -> Result<Vec<MemoryFact>> {
        super::turso::TursoClient::get_user_memory(self, user_id).await
    }

    async fn get_memory_by_category(
        &self,
        user_id: &str,
        category: &str,
    ) -> Result<Vec<MemoryFact>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT id, user_id, category, fact_key, fact_value, confidence, created_at, updated_at
                 FROM memory_facts WHERE user_id = ? AND category = ?",
                [user_id, category],
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

    async fn store_preference(&self, user_id: &str, preference: &Preference) -> Result<()> {
        super::turso::TursoClient::store_preference(self, user_id, preference).await
    }

    async fn get_user_preferences(&self, user_id: &str) -> Result<Vec<Preference>> {
        super::turso::TursoClient::get_user_preferences(self, user_id).await
    }

    async fn get_preference(
        &self,
        user_id: &str,
        category: &str,
        key: &str,
    ) -> Result<Option<Preference>> {
        let conn = self.operation_conn().await?;

        let mut rows = conn
            .query(
                "SELECT category, key, value, confidence FROM preferences
                 WHERE user_id = ? AND category = ? AND key = ?",
                [user_id, category, key],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to query preference: {}", e)))?;

        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| AppError::Database(e.to_string()))?
        {
            Ok(Some(Preference {
                category: row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
                key: row.get(1).map_err(|e| AppError::Database(e.to_string()))?,
                value: row.get(2).map_err(|e| AppError::Database(e.to_string()))?,
                confidence: row
                    .get::<f64>(3)
                    .map_err(|e| AppError::Database(e.to_string()))?
                    as f32,
            }))
        } else {
            Ok(None)
        }
    }
}
