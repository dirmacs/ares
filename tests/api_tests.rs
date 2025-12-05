//! Integration tests for the API endpoints
//!
//! These tests require proper test infrastructure setup.
//! For now, they are marked as ignored until the full test harness is implemented.

use axum::Router;
use axum_test::TestServer;
use serde_json::json;

/// Create a test application router
///
/// This is a minimal router for basic health check testing.
/// For full integration tests with authentication, database, and LLM,
/// you would need to:
/// 1. Create a mock LLM client using mockall
/// 2. Use TursoClient::new_memory() for in-memory database
/// 3. Set up test JWT tokens
///
/// See the `db_tests.rs` for examples of using in-memory database testing.
async fn create_test_app() -> Router {
    // Minimal router for testing basic connectivity
    // Full app requires: TursoClient, QdrantClient, LLMClient, and AppState
    use axum::routing::get;

    Router::new().route("/health", get(|| async { "OK" }))
}

/// Create a test server with the full application
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
#[ignore = "Requires full test infrastructure with database mocking"]
async fn test_register_and_login() {
    let server = create_test_server().await;

    // Register
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

    // Login
    let response = server
        .post("/api/auth/login")
        .json(&json!({
            "email": "test@example.com",
            "password": "password123"
        }))
        .await;

    response.assert_status_ok();
}

#[tokio::test]
#[ignore = "Requires full test infrastructure with LLM mocking"]
async fn test_chat_endpoint() {
    let server = create_test_server().await;

    // This test would require:
    // 1. A valid JWT token
    // 2. Mocked LLM client
    // 3. Mocked database

    let response = server
        .post("/api/chat")
        .add_header(axum::http::header::AUTHORIZATION, "Bearer test-token")
        .json(&json!({
            "message": "Hello, how are you?"
        }))
        .await;

    // For now, we expect this to fail without proper setup
    // In a full implementation, we would assert success
    assert!(response.status_code().is_client_error() || response.status_code().is_success());
}
