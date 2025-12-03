use axum_test::TestServer;
use serde_json::json;

#[tokio::test]
async fn test_health_check() {
    let app = create_test_app().await;
    let server = TestServer::new(app).unwrap();

    let response = server.get("/health").await;
    response.assert_status_ok();
    response.assert_text("OK");
}

#[tokio::test]
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
