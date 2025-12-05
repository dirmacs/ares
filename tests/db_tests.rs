//! Database integration tests
//!
//! These tests verify the TursoClient functionality using in-memory SQLite.

use chrono::Utc;

// Note: We test the database module through direct module access
// since it's re-exported through lib.rs

/// Test helper to create a TursoClient with in-memory database
async fn create_test_client() -> ares::db::TursoClient {
    ares::db::TursoClient::new_memory()
        .await
        .expect("Failed to create in-memory database")
}

#[tokio::test]
async fn test_create_memory_client() {
    let client = create_test_client().await;
    // If we get here without error, the client was created successfully
    // and the schema was initialized
    assert!(client.connection().is_ok());
}

#[tokio::test]
async fn test_create_local_client() {
    // Create a temporary file path for testing
    let temp_path = ":memory:";
    let client = ares::db::TursoClient::new_local(temp_path)
        .await
        .expect("Failed to create local database");

    assert!(client.connection().is_ok());
}

#[tokio::test]
async fn test_create_user() {
    let client = create_test_client().await;

    let result = client
        .create_user(
            "user-123",
            "test@example.com",
            "hashed_password_here",
            "Test User",
        )
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_create_duplicate_user_fails() {
    let client = create_test_client().await;

    // Create first user
    client
        .create_user(
            "user-123",
            "test@example.com",
            "hashed_password",
            "Test User",
        )
        .await
        .expect("First user creation should succeed");

    // Try to create user with same email
    let result = client
        .create_user(
            "user-456",
            "test@example.com",
            "different_password",
            "Another User",
        )
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_get_user_by_email() {
    let client = create_test_client().await;

    // Create a user
    client
        .create_user(
            "user-123",
            "findme@example.com",
            "hashed_password",
            "Find Me",
        )
        .await
        .expect("User creation should succeed");

    // Find the user
    let user = client
        .get_user_by_email("findme@example.com")
        .await
        .expect("Query should succeed");

    assert!(user.is_some());
    let user = user.unwrap();
    assert_eq!(user.id, "user-123");
    assert_eq!(user.email, "findme@example.com");
    assert_eq!(user.name, "Find Me");
}

#[tokio::test]
async fn test_get_nonexistent_user() {
    let client = create_test_client().await;

    let user = client
        .get_user_by_email("nonexistent@example.com")
        .await
        .expect("Query should succeed");

    assert!(user.is_none());
}

#[tokio::test]
async fn test_create_session() {
    let client = create_test_client().await;

    // Create a user first
    client
        .create_user("user-123", "test@example.com", "password", "Test")
        .await
        .expect("User creation should succeed");

    // Create a session
    let expires_at = Utc::now().timestamp() + 3600; // 1 hour from now
    let result = client
        .create_session("session-123", "user-123", "token_hash_here", expires_at)
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_create_conversation() {
    let client = create_test_client().await;

    // Create a user first
    client
        .create_user("user-123", "test@example.com", "password", "Test")
        .await
        .expect("User creation should succeed");

    // Create a conversation
    let result = client
        .create_conversation("conv-123", "user-123", Some("My Conversation"))
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_create_conversation_without_title() {
    let client = create_test_client().await;

    // Create a user first
    client
        .create_user("user-123", "test@example.com", "password", "Test")
        .await
        .expect("User creation should succeed");

    // Create a conversation without title
    let result = client
        .create_conversation("conv-123", "user-123", None)
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_add_and_get_messages() {
    let client = create_test_client().await;

    // Create user and conversation
    client
        .create_user("user-123", "test@example.com", "password", "Test")
        .await
        .unwrap();
    client
        .create_conversation("conv-123", "user-123", None)
        .await
        .unwrap();

    // Add messages
    client
        .add_message(
            "msg-1",
            "conv-123",
            ares::types::MessageRole::User,
            "Hello!",
        )
        .await
        .expect("Adding user message should succeed");

    client
        .add_message(
            "msg-2",
            "conv-123",
            ares::types::MessageRole::Assistant,
            "Hi there! How can I help you?",
        )
        .await
        .expect("Adding assistant message should succeed");

    client
        .add_message(
            "msg-3",
            "conv-123",
            ares::types::MessageRole::User,
            "What is the weather?",
        )
        .await
        .expect("Adding second user message should succeed");

    // Get conversation history
    let messages = client
        .get_conversation_history("conv-123")
        .await
        .expect("Getting history should succeed");

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].content, "Hello!");
    assert!(matches!(messages[0].role, ares::types::MessageRole::User));
    assert_eq!(messages[1].content, "Hi there! How can I help you?");
    assert!(matches!(
        messages[1].role,
        ares::types::MessageRole::Assistant
    ));
}

#[tokio::test]
async fn test_get_empty_conversation_history() {
    let client = create_test_client().await;

    // Get history for non-existent conversation
    let messages = client
        .get_conversation_history("nonexistent-conv")
        .await
        .expect("Getting history should succeed even if empty");

    assert!(messages.is_empty());
}

#[tokio::test]
async fn test_store_and_get_memory_fact() {
    let client = create_test_client().await;

    // Create user
    client
        .create_user("user-123", "test@example.com", "password", "Test")
        .await
        .unwrap();

    // Create and store a memory fact
    let fact = ares::types::MemoryFact {
        id: "fact-123".to_string(),
        user_id: "user-123".to_string(),
        category: "preferences".to_string(),
        fact_key: "favorite_color".to_string(),
        fact_value: "blue".to_string(),
        confidence: 0.95,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    client
        .store_memory_fact(&fact)
        .await
        .expect("Storing fact should succeed");

    // Get user memory
    let facts = client
        .get_user_memory("user-123")
        .await
        .expect("Getting memory should succeed");

    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].fact_key, "favorite_color");
    assert_eq!(facts[0].fact_value, "blue");
    assert!(facts[0].confidence > 0.9);
}

#[tokio::test]
async fn test_store_and_get_preferences() {
    let client = create_test_client().await;

    // Create user
    client
        .create_user("user-123", "test@example.com", "password", "Test")
        .await
        .unwrap();

    // Store preferences
    let pref1 = ares::types::Preference {
        category: "ui".to_string(),
        key: "theme".to_string(),
        value: "dark".to_string(),
        confidence: 1.0,
    };

    let pref2 = ares::types::Preference {
        category: "ui".to_string(),
        key: "font_size".to_string(),
        value: "14".to_string(),
        confidence: 0.8,
    };

    client
        .store_preference("user-123", &pref1)
        .await
        .expect("Storing preference should succeed");

    client
        .store_preference("user-123", &pref2)
        .await
        .expect("Storing second preference should succeed");

    // Get preferences
    let preferences = client
        .get_user_preferences("user-123")
        .await
        .expect("Getting preferences should succeed");

    assert_eq!(preferences.len(), 2);

    // Find theme preference
    let theme_pref = preferences.iter().find(|p| p.key == "theme");
    assert!(theme_pref.is_some());
    assert_eq!(theme_pref.unwrap().value, "dark");
}

#[tokio::test]
async fn test_preference_upsert() {
    let client = create_test_client().await;

    // Create user
    client
        .create_user("user-123", "test@example.com", "password", "Test")
        .await
        .unwrap();

    // Store initial preference
    let pref = ares::types::Preference {
        category: "settings".to_string(),
        key: "language".to_string(),
        value: "en".to_string(),
        confidence: 1.0,
    };

    client.store_preference("user-123", &pref).await.unwrap();

    // Update with new value (same category and key)
    let updated_pref = ares::types::Preference {
        category: "settings".to_string(),
        key: "language".to_string(),
        value: "es".to_string(),
        confidence: 1.0,
    };

    client
        .store_preference("user-123", &updated_pref)
        .await
        .unwrap();

    // Verify only one preference exists with updated value
    let preferences = client.get_user_preferences("user-123").await.unwrap();

    let lang_prefs: Vec<_> = preferences.iter().filter(|p| p.key == "language").collect();
    assert_eq!(lang_prefs.len(), 1);
    assert_eq!(lang_prefs[0].value, "es");
}

#[tokio::test]
async fn test_get_empty_user_memory() {
    let client = create_test_client().await;

    let facts = client
        .get_user_memory("nonexistent-user")
        .await
        .expect("Query should succeed");

    assert!(facts.is_empty());
}

#[tokio::test]
async fn test_get_empty_user_preferences() {
    let client = create_test_client().await;

    let preferences = client
        .get_user_preferences("nonexistent-user")
        .await
        .expect("Query should succeed");

    assert!(preferences.is_empty());
}

#[tokio::test]
async fn test_system_message_role() {
    let client = create_test_client().await;

    // Create user and conversation
    client
        .create_user("user-123", "test@example.com", "password", "Test")
        .await
        .unwrap();
    client
        .create_conversation("conv-123", "user-123", None)
        .await
        .unwrap();

    // Add system message
    client
        .add_message(
            "msg-1",
            "conv-123",
            ares::types::MessageRole::System,
            "You are a helpful assistant.",
        )
        .await
        .expect("Adding system message should succeed");

    let messages = client.get_conversation_history("conv-123").await.unwrap();

    assert_eq!(messages.len(), 1);
    assert!(matches!(messages[0].role, ares::types::MessageRole::System));
}

#[tokio::test]
async fn test_new_method_routing() {
    // Test that the new() method correctly routes to local vs remote
    // For a memory path, it should use local mode

    let client = ares::db::TursoClient::new(":memory:".to_string(), "".to_string())
        .await
        .expect("Should create local client for memory path");

    assert!(client.connection().is_ok());

    // Test with .db extension
    // This would create a file, so we use a temp path that won't persist
    // Actually, let's just test the memory case which is safe
}
