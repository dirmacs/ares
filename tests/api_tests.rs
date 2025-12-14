use axum::{Router, routing::get};
use axum_test::TestServer;
use serde_json::json;
use std::sync::Arc;
use std::collections::HashMap;

use ares::{
    AppState,
    auth::jwt::AuthService,
    db::TursoClient,
    llm::client::{LLMClientFactoryTrait, Provider},
    llm::{LLMClient, LLMResponse},
    types::{Result, ToolCall, ToolDefinition},
    AresConfigManager, ConfigBasedLLMFactory, ProviderRegistry,
    utils::toml_config::{AresConfig, ServerConfig as TomlServerConfig, AuthConfig as TomlAuthConfig, DatabaseConfig as TomlDatabaseConfig, ProviderConfig, ModelConfig, AgentConfig, RagConfig},
};
use async_trait::async_trait;
use futures::stream::{self, StreamExt};

// ============= Mock LLM Clients =============

/// Mock LLM client for testing with configurable responses
#[derive(Clone)]
struct MockLLMClient {
    response: String,
    tool_calls: Vec<ToolCall>,
    should_fail: bool,
}

impl MockLLMClient {
    fn new(response: &str) -> Self {
        Self {
            response: response.to_string(),
            tool_calls: vec![],
            should_fail: false,
        }
    }

    fn with_tool_calls(response: &str, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            response: response.to_string(),
            tool_calls,
            should_fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            response: String::new(),
            tool_calls: vec![],
            should_fail: true,
        }
    }
}

#[async_trait]
impl LLMClient for MockLLMClient {
    async fn generate(&self, _prompt: &str) -> Result<String> {
        if self.should_fail {
            return Err(ares::types::AppError::LLM("Mock LLM failure".to_string()));
        }
        Ok(self.response.clone())
    }

    async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
        if self.should_fail {
            return Err(ares::types::AppError::LLM("Mock LLM failure".to_string()));
        }
        Ok(self.response.clone())
    }

    async fn generate_with_history(&self, _messages: &[(String, String)]) -> Result<String> {
        if self.should_fail {
            return Err(ares::types::AppError::LLM("Mock LLM failure".to_string()));
        }
        Ok(self.response.clone())
    }

    async fn generate_with_tools(
        &self,
        _prompt: &str,
        _tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        if self.should_fail {
            return Err(ares::types::AppError::LLM("Mock LLM failure".to_string()));
        }

        let finish_reason = if self.tool_calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        };

        Ok(LLMResponse {
            content: self.response.clone(),
            tool_calls: self.tool_calls.clone(),
            finish_reason: finish_reason.to_string(),
        })
    }

    async fn stream(
        &self,
        _prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        if self.should_fail {
            return Err(ares::types::AppError::LLM("Mock LLM failure".to_string()));
        }

        let response = self.response.clone();
        // Split response into chunks for streaming simulation
        let chunks: Vec<String> = response
            .chars()
            .collect::<Vec<_>>()
            .chunks(5)
            .map(|c| c.iter().collect())
            .collect();

        let stream = stream::iter(chunks.into_iter().map(Ok));
        Ok(Box::new(stream.boxed()))
    }

    fn model_name(&self) -> &str {
        "mock-model"
    }
}

// ============= Mock LLM Factory =============
// This factory can be used for tests that need complete isolation from external services.
// Currently unused but preserved for future test scenarios requiring mock LLM responses.

#[allow(dead_code)]
struct MockLLMFactory {
    provider: Provider,
    client: Arc<MockLLMClient>,
}

#[allow(dead_code)]
impl MockLLMFactory {
    fn new(client: MockLLMClient) -> Self {
        Self {
            provider: Provider::Ollama {
                base_url: "http://localhost:11434".to_string(),
                model: "mock".to_string(),
            },
            client: Arc::new(client),
        }
    }
}

#[async_trait]
impl LLMClientFactoryTrait for MockLLMFactory {
    fn default_provider(&self) -> &Provider {
        &self.provider
    }

    async fn create_default(&self) -> Result<Box<dyn LLMClient>> {
        Ok(Box::new((*self.client).clone()))
    }

    async fn create_with_provider(&self, _provider: Provider) -> Result<Box<dyn LLMClient>> {
        Ok(Box::new((*self.client).clone()))
    }
}

// ============= Test Helpers =============

/// Create a test app with in-memory database
async fn create_test_app() -> Router {
    // Create in-memory database
    let turso = TursoClient::new_memory()
        .await
        .expect("Failed to create in-memory database");

    // Create a test user for auth middleware
    turso
        .create_user(
            "test-user",
            "testuser@example.com",
            "dummy_hash",
            "Test User",
        )
        .await
        .expect("Failed to create test user");

    // Create auth service with test secret
    let auth_service = AuthService::new(
        "test_jwt_secret_key_for_testing_only".to_string(),
        900,    // 15 minutes access token
        604800, // 7 days refresh token
    );

    // Create test TOML config
    let mut providers = HashMap::new();
    providers.insert("ollama-local".to_string(), ProviderConfig::Ollama {
        base_url: "http://localhost:11434".to_string(),
        default_model: "llama3.2".to_string(),
    });

    let mut models = HashMap::new();
    models.insert("default".to_string(), ModelConfig {
        provider: "ollama-local".to_string(),
        model: "llama3.2".to_string(),
        temperature: 0.7,
        max_tokens: 512,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
    });

    let mut agents = HashMap::new();
    agents.insert("router".to_string(), AgentConfig {
        model: "default".to_string(),
        system_prompt: Some("You are a routing agent.".to_string()),
        tools: vec![],
        max_tool_iterations: 10,
        parallel_tools: false,
        extra: HashMap::new(),
    });

    let ares_config = AresConfig {
        server: TomlServerConfig {
            host: "127.0.0.1".to_string(),
            port: 3000,
            log_level: "debug".to_string(),
        },
        auth: TomlAuthConfig {
            jwt_secret_env: "TEST_JWT_SECRET".to_string(),
            jwt_access_expiry: 900,
            jwt_refresh_expiry: 604800,
            api_key_env: "TEST_API_KEY".to_string(),
        },
        database: TomlDatabaseConfig {
            url: ":memory:".to_string(),
            turso_url_env: None,
            turso_token_env: None,
            qdrant: None,
        },
        providers,
        models,
        tools: HashMap::new(),
        agents,
        workflows: HashMap::new(),
        rag: RagConfig::default(),
    };

    // Create config manager (without file watcher for tests)
    let config_manager = Arc::new(AresConfigManager::from_config(ares_config));

    // Create provider registry from config
    let provider_registry = Arc::new(ProviderRegistry::from_config(&config_manager.config()));

    // Create config-based LLM factory
    let llm_factory = Arc::new(ConfigBasedLLMFactory::new(provider_registry.clone(), "default"));

    let state = AppState {
        config_manager,
        turso: Arc::new(turso),
        llm_factory,
        provider_registry,
        auth_service: Arc::new(auth_service),
    };

    // Build a minimal router for testing
    Router::new()
        .route("/health", get(|| async { "OK" }))
        .nest(
            "/api",
            ares::api::routes::create_router(state.auth_service.clone()),
        )
        .with_state(state)
}

/// Create a test server
async fn create_test_server() -> TestServer {
    let app = create_test_app().await;
    TestServer::new(app).expect("Failed to create test server")
}

// ============= Health Check Tests =============

#[tokio::test]
async fn test_health_check() {
    let server = create_test_server().await;

    let response = server.get("/health").await;
    response.assert_status_ok();
    response.assert_text("OK");
}

#[tokio::test]
async fn test_health_check_multiple_times() {
    let server = create_test_server().await;

    for _ in 0..5 {
        let response = server.get("/health").await;
        response.assert_status_ok();
    }
}

// ============= Authentication Tests =============

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
async fn test_register_invalid_email() {
    let server = create_test_server().await;

    // Note: The current API doesn't validate email format strictly
    // This test documents current behavior - registration succeeds with any string
    let response = server
        .post("/api/auth/register")
        .json(&json!({
            "email": "notanemail",
            "password": "password123",
            "name": "Test User"
        }))
        .await;

    // Current API accepts any string as email (no format validation)
    response.assert_status_ok();
}

#[tokio::test]
async fn test_register_empty_name() {
    let server = create_test_server().await;

    // Note: The current API doesn't validate empty names
    // This test documents current behavior
    let response = server
        .post("/api/auth/register")
        .json(&json!({
            "email": "emptyname@example.com",
            "password": "password123",
            "name": ""
        }))
        .await;

    // Current API accepts empty name (no validation)
    response.assert_status_ok();
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
async fn test_refresh_token_invalid() {
    let server = create_test_server().await;

    // Use invalid refresh token
    let response = server
        .post("/api/auth/refresh")
        .json(&json!({
            "refresh_token": "invalid_token_here"
        }))
        .await;

    response.assert_status_unauthorized();
}

#[tokio::test]
async fn test_multiple_logins() {
    let server = create_test_server().await;

    // Register
    let response = server
        .post("/api/auth/register")
        .json(&json!({
            "email": "multilogin@example.com",
            "password": "password123",
            "name": "Test User"
        }))
        .await;

    response.assert_status_ok();

    // Login multiple times, each should succeed
    for i in 0..3 {
        let response = server
            .post("/api/auth/login")
            .json(&json!({
                "email": "multilogin@example.com",
                "password": "password123"
            }))
            .await;

        response.assert_status_ok();
        let body: serde_json::Value = response.json();
        assert!(body["access_token"].is_string(), "Login {} failed", i + 1);
    }
}

// ============= Agents Tests =============

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

#[tokio::test]
async fn test_agents_list_structure() {
    let server = create_test_server().await;

    let response = server.get("/api/agents").await;
    response.assert_status_ok();

    let body: Vec<serde_json::Value> = response.json();

    // Each agent should have name and description
    for agent in body {
        assert!(agent["name"].is_string());
        assert!(agent["description"].is_string());
    }
}

/// Test chat endpoint with live Ollama server
/// This test validates the full chat flow including authentication, routing, and LLM response.
/// Run with: cargo test test_chat_endpoint_with_live_ollama -- --ignored
#[tokio::test]
#[ignore = "requires running Ollama server"]
async fn test_chat_endpoint_with_live_ollama() {
    let server = create_test_server().await;

    // Register to obtain a bearer token
    let register = server
        .post("/api/auth/register")
        .json(&json!({
            "email": "chatuser@example.com",
            "password": "password123",
            "name": "Chat User"
        }))
        .await;

    register.assert_status_ok();
    let body: serde_json::Value = register.json();
    let token = body["access_token"].as_str().unwrap();

    let response = server
        .post("/api/chat")
        .add_header("Authorization", format!("Bearer {}", token))
        .json(&json!({
            "message": "Hello agent!",
            "agent_type": "product"
        }))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    
    // Verify response structure (don't assert specific content as LLM responses vary)
    assert_eq!(body["agent"], "Product");
    assert!(body["response"].is_string(), "Response should be a string");
    assert!(!body["response"].as_str().unwrap().is_empty(), "Response should not be empty");
    assert!(body["context_id"].is_string(), "context_id should be a string");
}

// ============= Mock LLM Tests =============

#[tokio::test]
async fn test_mock_llm_generate() {
    let client = MockLLMClient::new("Hello, world!");
    let result = client.generate("test prompt").await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "Hello, world!");
}

#[tokio::test]
async fn test_mock_llm_with_system() {
    let client = MockLLMClient::new("System response");
    let result = client
        .generate_with_system("You are helpful", "Hello")
        .await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "System response");
}

#[tokio::test]
async fn test_mock_llm_with_history() {
    let client = MockLLMClient::new("History response");
    let messages = vec![
        ("user".to_string(), "Hello".to_string()),
        ("assistant".to_string(), "Hi!".to_string()),
    ];
    let result = client.generate_with_history(&messages).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "History response");
}

#[tokio::test]
async fn test_mock_llm_with_tools_no_calls() {
    let client = MockLLMClient::new("Tool response");
    let tools = vec![ToolDefinition {
        name: "calculator".to_string(),
        description: "Math operations".to_string(),
        parameters: json!({}),
    }];

    let result = client.generate_with_tools("Calculate 2+2", &tools).await;
    assert!(result.is_ok());

    let response = result.unwrap();
    assert_eq!(response.content, "Tool response");
    assert_eq!(response.finish_reason, "stop");
    assert!(response.tool_calls.is_empty());
}

#[tokio::test]
async fn test_mock_llm_with_tools_with_calls() {
    let tool_calls = vec![ToolCall {
        id: "call-1".to_string(),
        name: "calculator".to_string(),
        arguments: json!({"operation": "add", "a": 2, "b": 2}),
    }];

    let client = MockLLMClient::with_tool_calls("I'll calculate that", tool_calls);
    let tools = vec![ToolDefinition {
        name: "calculator".to_string(),
        description: "Math operations".to_string(),
        parameters: json!({}),
    }];

    let result = client.generate_with_tools("Calculate 2+2", &tools).await;
    assert!(result.is_ok());

    let response = result.unwrap();
    assert_eq!(response.finish_reason, "tool_calls");
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name, "calculator");
}

#[tokio::test]
async fn test_mock_llm_streaming() {
    let client = MockLLMClient::new("Hello streaming world!");
    let result = client.stream("test").await;
    assert!(result.is_ok());

    let mut stream = result.unwrap();
    let mut collected = String::new();

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => collected.push_str(&chunk),
            Err(_) => break,
        }
    }

    assert_eq!(collected, "Hello streaming world!");
}

#[tokio::test]
async fn test_mock_llm_failure() {
    let client = MockLLMClient::failing();

    let result = client.generate("test").await;
    assert!(result.is_err());

    let result = client.generate_with_system("sys", "test").await;
    assert!(result.is_err());

    let result = client.generate_with_history(&[]).await;
    assert!(result.is_err());

    let result = client.generate_with_tools("test", &[]).await;
    assert!(result.is_err());

    let result = client.stream("test").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_mock_llm_model_name() {
    let client = MockLLMClient::new("test");
    assert_eq!(client.model_name(), "mock-model");
}

// ============= Tool Calling Integration Tests =============

#[tokio::test]
async fn test_multiple_tool_calls() {
    let tool_calls = vec![
        ToolCall {
            id: "call-1".to_string(),
            name: "get_weather".to_string(),
            arguments: json!({"city": "London"}),
        },
        ToolCall {
            id: "call-2".to_string(),
            name: "get_time".to_string(),
            arguments: json!({"timezone": "UTC"}),
        },
        ToolCall {
            id: "call-3".to_string(),
            name: "search".to_string(),
            arguments: json!({"query": "news"}),
        },
    ];

    let client = MockLLMClient::with_tool_calls("Processing multiple tools", tool_calls);
    let tools: Vec<ToolDefinition> = vec![];

    let result = client
        .generate_with_tools("What's the weather, time, and news?", &tools)
        .await;
    assert!(result.is_ok());

    let response = result.unwrap();
    assert_eq!(response.tool_calls.len(), 3);
    assert_eq!(response.tool_calls[0].name, "get_weather");
    assert_eq!(response.tool_calls[1].name, "get_time");
    assert_eq!(response.tool_calls[2].name, "search");
}

#[tokio::test]
async fn test_tool_definition_structure() {
    let tool = ToolDefinition {
        name: "complex_tool".to_string(),
        description: "A complex tool with nested parameters".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "The name"},
                "count": {"type": "integer", "minimum": 0},
                "options": {
                    "type": "object",
                    "properties": {
                        "verbose": {"type": "boolean"},
                        "format": {"type": "string", "enum": ["json", "text"]}
                    }
                }
            },
            "required": ["name"]
        }),
    };

    assert_eq!(tool.name, "complex_tool");
    assert!(tool.parameters["properties"]["options"].is_object());
}

#[tokio::test]
async fn test_tool_call_complex_arguments() {
    let tool_call = ToolCall {
        id: "call-complex".to_string(),
        name: "complex_tool".to_string(),
        arguments: json!({
            "string_arg": "hello",
            "number_arg": 42,
            "float_arg": 2.75,
            "bool_arg": true,
            "null_arg": null,
            "array_arg": [1, 2, 3],
            "object_arg": {"nested": "value", "deep": {"deeper": true}}
        }),
    };

    assert_eq!(tool_call.arguments["string_arg"], "hello");
    assert_eq!(tool_call.arguments["number_arg"], 42);
    assert!((tool_call.arguments["float_arg"].as_f64().unwrap() - 2.75).abs() < 0.001);
    assert!(tool_call.arguments["bool_arg"].as_bool().unwrap());
    assert!(tool_call.arguments["null_arg"].is_null());
    assert_eq!(
        tool_call.arguments["array_arg"].as_array().unwrap().len(),
        3
    );
    assert_eq!(tool_call.arguments["object_arg"]["deep"]["deeper"], true);
}

// ============= Edge Case Tests =============

#[tokio::test]
async fn test_empty_prompt() {
    let client = MockLLMClient::new("Response to empty");
    let result = client.generate("").await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_very_long_prompt() {
    let client = MockLLMClient::new("Response to long prompt");
    let long_prompt = "test ".repeat(10000);
    let result = client.generate(&long_prompt).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_unicode_content() {
    let client = MockLLMClient::new("Response with unicode: 你好世界 🌍 مرحبا");
    let result = client
        .generate("Hello in multiple languages: 你好 مرحبا")
        .await;
    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.contains("你好世界"));
    assert!(response.contains("🌍"));
}

#[tokio::test]
async fn test_special_characters() {
    let client = MockLLMClient::new("Response with special chars: <>&\"'\\");
    let prompt = r#"Test with "quotes", 'apostrophes', \backslash, <angle>, &ampersand"#;
    let result = client.generate(prompt).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_newlines_in_content() {
    let client = MockLLMClient::new("Line 1\nLine 2\nLine 3");
    let result = client.generate("Give me multiple lines").await;
    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.contains('\n'));
}

#[tokio::test]
async fn test_empty_history() {
    let client = MockLLMClient::new("Response to empty history");
    let history: Vec<(String, String)> = vec![];
    let result = client.generate_with_history(&history).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_large_history() {
    let client = MockLLMClient::new("Response after long history");
    let history: Vec<(String, String)> = (0..100)
        .map(|i| {
            if i % 2 == 0 {
                ("user".to_string(), format!("Message {}", i))
            } else {
                ("assistant".to_string(), format!("Response {}", i))
            }
        })
        .collect();

    let result = client.generate_with_history(&history).await;
    assert!(result.is_ok());
}

// ============= Response Structure Tests =============

#[tokio::test]
async fn test_auth_response_structure() {
    let server = create_test_server().await;

    let response = server
        .post("/api/auth/register")
        .json(&json!({
            "email": "structure@example.com",
            "password": "password123",
            "name": "Test User"
        }))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();

    // Verify response has required fields
    assert!(body["access_token"].is_string());
    assert!(body["refresh_token"].is_string());
    assert!(body["expires_in"].is_number());

    // Verify tokens are not empty
    assert!(!body["access_token"].as_str().unwrap().is_empty());
    assert!(!body["refresh_token"].as_str().unwrap().is_empty());

    // Verify expires_in is positive
    assert!(body["expires_in"].as_i64().unwrap() > 0);
}

// ============= Input Validation Tests =============

#[tokio::test]
async fn test_missing_required_fields() {
    let server = create_test_server().await;

    // Missing password - API returns 422 Unprocessable Entity for missing fields
    let response = server
        .post("/api/auth/register")
        .json(&json!({
            "email": "missing@example.com",
            "name": "Test User"
        }))
        .await;

    // Axum returns 422 for deserialization errors (missing fields)
    response.assert_status_unprocessable_entity();

    // Missing email
    let response = server
        .post("/api/auth/register")
        .json(&json!({
            "password": "password123",
            "name": "Test User"
        }))
        .await;

    response.assert_status_unprocessable_entity();
}

#[tokio::test]
async fn test_extra_fields_ignored() {
    let server = create_test_server().await;

    // Request with extra fields that should be ignored
    let response = server
        .post("/api/auth/register")
        .json(&json!({
            "email": "extrafields@example.com",
            "password": "password123",
            "name": "Test User",
            "extra_field": "should be ignored",
            "another_extra": 12345
        }))
        .await;

    // Should still succeed
    response.assert_status_ok();
}
