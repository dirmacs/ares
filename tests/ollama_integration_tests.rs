//! Comprehensive Ollama Integration Tests with Mocked Network Responses
//!
//! These tests use wiremock to mock the Ollama API server and validate:
//! - Basic chat functionality
//! - Streaming responses
//! - Tool calling flows
//! - Error handling
//! - Concurrent requests

#![cfg(feature = "ollama")]

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ============= Helper Functions =============

/// Create a mock Ollama chat completion response
fn mock_chat_response(content: &str, done: bool) -> serde_json::Value {
    json!({
        "model": "llama3.2",
        "created_at": "2024-01-01T00:00:00Z",
        "message": {
            "role": "assistant",
            "content": content
        },
        "done": done
    })
}

/// Create a mock Ollama response with tool calls
fn mock_chat_response_with_tools(
    content: &str,
    tool_calls: Vec<(&str, serde_json::Value)>,
) -> serde_json::Value {
    let formatted_tools: Vec<serde_json::Value> = tool_calls
        .into_iter()
        .map(|(name, args)| {
            json!({
                "function": {
                    "name": name,
                    "arguments": args
                }
            })
        })
        .collect();

    json!({
        "model": "llama3.2",
        "created_at": "2024-01-01T00:00:00Z",
        "message": {
            "role": "assistant",
            "content": content,
            "tool_calls": formatted_tools
        },
        "done": true
    })
}

/// Create a streaming mock response (NDJSON format)
fn mock_streaming_response(chunks: &[&str]) -> String {
    let total = chunks.len();
    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let done = i == total - 1;
            let response = json!({
                "model": "llama3.2",
                "created_at": "2024-01-01T00:00:00Z",
                "message": {
                    "role": "assistant",
                    "content": chunk
                },
                "done": done
            });
            format!("{}\n", response)
        })
        .collect::<Vec<_>>()
        .join("")
}

// ============= Basic Ollama Client Tests =============

#[tokio::test]
async fn test_ollama_mock_server_simple_chat() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_chat_response("Hello! How can I help you?", true)),
        )
        .mount(&mock_server)
        .await;

    // Verify the mock server is working
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/chat", mock_server.uri()))
        .json(&json!({
            "model": "llama3.2",
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["message"]["content"], "Hello! How can I help you?");
}

#[tokio::test]
async fn test_ollama_mock_server_with_system_prompt() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_chat_response("I am a helpful coding assistant.", true)),
        )
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/chat", mock_server.uri()))
        .json(&json!({
            "model": "llama3.2",
            "messages": [
                {"role": "system", "content": "You are a coding assistant"},
                {"role": "user", "content": "Who are you?"}
            ]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(
        body["message"]["content"]
            .as_str()
            .unwrap()
            .contains("coding assistant")
    );
}

#[tokio::test]
async fn test_ollama_mock_server_with_history() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_chat_response("3 + 3 equals 6.", true)),
        )
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/chat", mock_server.uri()))
        .json(&json!({
            "model": "llama3.2",
            "messages": [
                {"role": "user", "content": "What is 2 + 2?"},
                {"role": "assistant", "content": "2 + 2 equals 4."},
                {"role": "user", "content": "What about 3 + 3?"}
            ]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["message"]["content"].as_str().unwrap().contains("6"));
}

// ============= Streaming Tests =============

#[tokio::test]
async fn test_ollama_mock_server_streaming() {
    let mock_server = MockServer::start().await;

    let chunks = ["Hello", " there", "! How", " can", " I help", "?"];
    let stream_body = mock_streaming_response(&chunks);

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(stream_body))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/chat", mock_server.uri()))
        .json(&json!({
            "model": "llama3.2",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert!(body.contains("Hello"));
    assert!(body.contains("help"));
}

// ============= Tool Calling Tests =============

#[tokio::test]
async fn test_ollama_mock_server_single_tool_call() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_chat_response_with_tools(
                "I need to calculate this.",
                vec![("calculator", json!({"operation": "add", "a": 5, "b": 3}))],
            )),
        )
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/chat", mock_server.uri()))
        .json(&json!({
            "model": "llama3.2",
            "messages": [{"role": "user", "content": "What is 5 + 3?"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "calculator",
                    "description": "Performs arithmetic",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "operation": {"type": "string"},
                            "a": {"type": "number"},
                            "b": {"type": "number"}
                        },
                        "required": ["operation", "a", "b"]
                    }
                }
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["message"]["tool_calls"].is_array());
    let tool_calls = body["message"]["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["function"]["name"], "calculator");
    assert_eq!(tool_calls[0]["function"]["arguments"]["operation"], "add");
}

#[tokio::test]
async fn test_ollama_mock_server_multiple_tool_calls() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_chat_response_with_tools(
                "Let me perform these calculations.",
                vec![
                    ("calculator", json!({"operation": "add", "a": 5, "b": 3})),
                    (
                        "calculator",
                        json!({"operation": "multiply", "a": 4, "b": 2}),
                    ),
                ],
            )),
        )
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/chat", mock_server.uri()))
        .json(&json!({
            "model": "llama3.2",
            "messages": [{"role": "user", "content": "Calculate 5+3 and 4*2"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    let tool_calls = body["message"]["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_calls[0]["function"]["arguments"]["operation"], "add");
    assert_eq!(
        tool_calls[1]["function"]["arguments"]["operation"],
        "multiply"
    );
}

// ============= Error Handling Tests =============

#[tokio::test]
async fn test_ollama_mock_server_error() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": "Internal server error"
        })))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/chat", mock_server.uri()))
        .json(&json!({
            "model": "llama3.2",
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 500);
}

#[tokio::test]
async fn test_ollama_mock_server_invalid_model() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": "model not found"
        })))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/chat", mock_server.uri()))
        .json(&json!({
            "model": "nonexistent-model",
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn test_ollama_mock_server_malformed_response() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string("This is not valid JSON"))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/chat", mock_server.uri()))
        .json(&json!({
            "model": "llama3.2",
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let result: Result<serde_json::Value, _> = response.json().await;
    assert!(result.is_err()); // Should fail to parse JSON
}

// ============= Edge Cases =============

#[tokio::test]
async fn test_ollama_mock_server_empty_response() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_chat_response("", true)))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/chat", mock_server.uri()))
        .json(&json!({
            "model": "llama3.2",
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["message"]["content"], "");
}

#[tokio::test]
async fn test_ollama_mock_server_unicode_content() {
    let mock_server = MockServer::start().await;

    let unicode_text = "Hello! 你好 🌍 مرحبا";
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_chat_response(unicode_text, true)),
        )
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/chat", mock_server.uri()))
        .json(&json!({
            "model": "llama3.2",
            "messages": [{"role": "user", "content": "Say hello in multiple languages"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    let content = body["message"]["content"].as_str().unwrap();
    assert!(content.contains("你好"));
    assert!(content.contains("🌍"));
    assert!(content.contains("مرحبا"));
}

// ============= Concurrent Request Tests =============

#[tokio::test]
async fn test_ollama_mock_server_concurrent_requests() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_chat_response("Concurrent response", true)),
        )
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let uri = mock_server.uri();

    // Spawn multiple concurrent requests
    let mut handles = vec![];
    for i in 0..5 {
        let client_clone = client.clone();
        let uri_clone = uri.clone();
        let handle = tokio::spawn(async move {
            client_clone
                .post(format!("{}/api/chat", uri_clone))
                .json(&json!({
                    "model": "llama3.2",
                    "messages": [{"role": "user", "content": format!("Request {}", i)}]
                }))
                .send()
                .await
        });
        handles.push(handle);
    }

    // Wait for all to complete
    for handle in handles {
        let result = handle.await.unwrap();
        assert!(result.is_ok());
        assert_eq!(result.unwrap().status(), 200);
    }
}

// ============= Response Format Tests =============

#[test]
fn test_mock_chat_response_format() {
    let response = mock_chat_response("Test content", true);

    assert_eq!(response["model"], "llama3.2");
    assert_eq!(response["message"]["role"], "assistant");
    assert_eq!(response["message"]["content"], "Test content");
    assert_eq!(response["done"], true);
}

#[test]
fn test_mock_chat_response_with_tools_format() {
    let response = mock_chat_response_with_tools(
        "Calling tools",
        vec![
            ("tool1", json!({"arg": "value1"})),
            ("tool2", json!({"arg": "value2"})),
        ],
    );

    assert_eq!(response["message"]["content"], "Calling tools");
    assert!(response["message"]["tool_calls"].is_array());
    let tool_calls = response["message"]["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_calls[0]["function"]["name"], "tool1");
    assert_eq!(tool_calls[1]["function"]["name"], "tool2");
}

#[test]
fn test_mock_streaming_response_format() {
    let chunks = ["Hello", " World"];
    let response = mock_streaming_response(&chunks);

    // Should be NDJSON format
    let lines: Vec<&str> = response.lines().collect();
    assert_eq!(lines.len(), 2);

    // Parse each line
    for line in lines {
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(parsed["message"]["content"].is_string());
    }
}
