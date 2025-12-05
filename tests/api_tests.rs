use axum::{routing::get, Router};
use axum_test::TestServer;
use serde_json::json;
use std::sync::Arc;

use ares::{
    auth::jwt::AuthService,
    db::TursoClient,
    llm::{LLMClient, LLMResponse},
    types::{Result, ToolDefinition},
    AppState,
};
use async_trait::async_trait;

/// Mock LLM client for testing
struct _MockLLMClient;

#[async_trait]
impl LLMClient for _MockLLMClient {
    async fn generate(&self, _prompt: &str) -> Result<String> {
        Ok("Mock response".to_string())
    }

    async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
        Ok("Mock response".to_string())
    }

    async fn generate_with_history(&self, _messages: &[(String, String)]) -> Result<String> {
        Ok("Mock response".to_string())
    }

    async fn generate_with_tools(
        &self,
        _prompt: &str,
        _tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        Ok(LLMResponse {
            content: "Mock response".to_string(),
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
        })
    }

    async fn stream(
        &self,
        _prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        use futures::stream::{self, StreamExt};
        Ok(Box::new(
            stream::once(async { Ok("Mock response".to_string()) }).boxed(),
        ))
    }

    fn model_name(&self) -> &str {
        "mock-model"
    }
}

/// Mock LLM client factory

/// Create a test app with in-memory database
async fn create_test_app() -> Router {
    // Create in-memory database
    let turso = TursoClient::new_memory()
        .await
        .expect("Failed to create in-memory database");

    // Create auth service with test secret
    let auth_service = AuthService::new(
        "test_jwt_secret_key_for_testing_only".to_string(),
        900,    // 15 minutes access token
        604800, // 7 days refresh token
    );

    // Create test config
    let config = ares::utils::config::Config {
        server: ares::utils::config::ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 3000,
        },
        database: ares::utils::config::DatabaseConfig {
            turso_url: ":memory:".to_string(),
            turso_auth_token: "".to_string(),
            qdrant_url: "http://localhost:6334".to_string(),
            qdrant_api_key: None,
        },
        llm: ares::utils::config::LLMConfig {
            openai_api_key: None,
            anthropic_api_key: None,
            ollama_url: "http://localhost:11434".to_string(),
        },
        auth: ares::utils::config::AuthConfig {
            jwt_secret: "test_jwt_secret_key_for_testing_only".to_string(),
            jwt_access_expiry: 900,
            jwt_refresh_expiry: 604800,
            api_key: "test_api_key".to_string(),
        },
        rag: ares::utils::config::RAGConfig {
            embedding_model: "BAAI/bge-small-en-v1.5".to_string(),
            chunk_size: 1000,
            chunk_overlap: 200,
        },
    };

    // Create mock LLM factory - we need to use the real one but it won't be called for auth tests
    let llm_factory = ares::llm::LLMClientFactory::new(ares::llm::Provider::Anthropic {
        api_key: "mock".to_string(),
        model: "mock".to_string(),
    });

    let state = AppState {
        config: Arc::new(config),
        turso: Arc::new(turso),
        llm_factory: Arc::new(llm_factory),
        auth_service: Arc::new(auth_service),
    };

    // Build a minimal router for testing
    Router::new()
        .route("/health", get(|| async { "OK" }))
        .nest("/api", ares::api::routes::create_router())
        .with_state(state)
}

/// Create a test server
async fn create_test_server() -> TestServer {
    let app = create_test_app().await;
    TestServer::new(app).expect("Failed to create test server")
}

#[tokio::test]
async fn test_health_check() {
    let server = create_test_server().await;

    let response = server.get("/health").await;
    response.assert_status_ok();
    response.assert_text("OK");
}

#[tokio::test]
async fn test_register_user() {
    let server = create_test_server().await;

    let response = server
        .post("/api/auth/register")
        .json(&json!({
            "email": "test@example.com",
            "password": "password123",
            "name": "Test User"
        }))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert!(body["access_token"].is_string());
    assert!(body["refresh_token"].is_string());
    assert!(body["expires_in"].is_number());
}

#[tokio::test]
async fn test_register_and_login() {
    let server = create_test_server().await;

    // Register
    let response = server
        .post("/api/auth/register")
        .json(&json!({
            "email": "login_test@example.com",
            "password": "password123",
            "name": "Test User"
        }))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert!(body["access_token"].is_string());

    // Login with the same credentials
    let response = server
        .post("/api/auth/login")
        .json(&json!({
            "email": "login_test@example.com",
            "password": "password123"
        }))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert!(body["access_token"].is_string());
    assert!(body["refresh_token"].is_string());
}

#[tokio::test]
async fn test_register_duplicate_user() {
    let server = create_test_server().await;

    // Register first user
    let response = server
        .post("/api/auth/register")
        .json(&json!({
            "email": "duplicate@example.com",
            "password": "password123",
            "name": "Test User"
        }))
        .await;

    response.assert_status_ok();

    // Try to register with same email
    let response = server
        .post("/api/auth/register")
        .json(&json!({
            "email": "duplicate@example.com",
            "password": "password456",
            "name": "Another User"
        }))
        .await;

    response.assert_status_bad_request();
}

#[tokio::test]
async fn test_login_invalid_credentials() {
    let server = create_test_server().await;

    // Try to login without registering
    let response = server
        .post("/api/auth/login")
        .json(&json!({
            "email": "nonexistent@example.com",
            "password": "password123"
        }))
        .await;

    response.assert_status_unauthorized();
}

#[tokio::test]
async fn test_login_wrong_password() {
    let server = create_test_server().await;

    // Register
    let response = server
        .post("/api/auth/register")
        .json(&json!({
            "email": "wrongpass@example.com",
            "password": "correct_password",
            "name": "Test User"
        }))
        .await;

    response.assert_status_ok();

    // Login with wrong password
    let response = server
        .post("/api/auth/login")
        .json(&json!({
            "email": "wrongpass@example.com",
            "password": "wrong_password"
        }))
        .await;

    response.assert_status_unauthorized();
}

#[tokio::test]
async fn test_register_short_password() {
    let server = create_test_server().await;

    // Try to register with short password (less than 8 characters)
    let response = server
        .post("/api/auth/register")
        .json(&json!({
            "email": "shortpass@example.com",
            "password": "short",
            "name": "Test User"
        }))
        .await;

    response.assert_status_bad_request();
}

#[tokio::test]
async fn test_refresh_token() {
    let server = create_test_server().await;

    // Register to get tokens
    let response = server
        .post("/api/auth/register")
        .json(&json!({
            "email": "refresh@example.com",
            "password": "password123",
            "name": "Test User"
        }))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    let refresh_token = body["refresh_token"].as_str().unwrap();

    // Use refresh token to get new tokens
    let response = server
        .post("/api/auth/refresh")
        .json(&json!({
            "refresh_token": refresh_token
        }))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert!(body["access_token"].is_string());
    assert!(body["refresh_token"].is_string());
}

#[tokio::test]
async fn test_agents_list() {
    let server = create_test_server().await;

    let response = server.get("/api/agents").await;
    response.assert_status_ok();

    let body: Vec<serde_json::Value> = response.json();
    assert!(!body.is_empty());

    // Check that expected agent types are present
    let agent_names: Vec<&str> = body.iter().filter_map(|a| a["name"].as_str()).collect();

    assert!(agent_names.contains(&"Product Agent"));
    assert!(agent_names.contains(&"Invoice Agent"));
    assert!(agent_names.contains(&"Sales Agent"));
    assert!(agent_names.contains(&"Finance Agent"));
    assert!(agent_names.contains(&"HR Agent"));
}
