//! Live Ollama Integration Tests
//!
//! These tests connect to a REAL Ollama instance and are **ignored by default**.
//!
//! To run these tests, you need:
//! 1. A running Ollama server (default: http://localhost:11434)
//! 2. The `ministral-3:3b` model pulled (or set OLLAMA_MODEL env var)
//!
//! # Running the tests
//!
//! ```bash
//! # Run with default Ollama URL (http://localhost:11434)
//! OLLAMA_LIVE_TESTS=1 cargo test --test ollama_live_tests -- --ignored
//!
//! # Run with custom Ollama URL
//! OLLAMA_URL=http://192.168.1.100:11434 OLLAMA_LIVE_TESTS=1 cargo test --test ollama_live_tests -- --ignored
//!
//! # Run with a specific model
//! OLLAMA_MODEL=mistral OLLAMA_LIVE_TESTS=1 cargo test --test ollama_live_tests -- --ignored
//! ```

#![cfg(feature = "ollama")]

use ares::llm::{LLMClient, Provider};
use futures::StreamExt;

// ============= Helper Functions =============

/// Check if live tests should run
fn should_run_live_tests() -> bool {
    std::env::var("OLLAMA_LIVE_TESTS").is_ok()
}

/// Get the Ollama URL from environment or use default
fn get_ollama_url() -> String {
    std::env::var("OLLAMA_URL")
        .or_else(|_| std::env::var("OLLAMA_BASE_URL"))
        .unwrap_or_else(|_| "http://localhost:11434".to_string())
}

/// Get the model name from environment or use default
fn get_model() -> String {
    std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "ministral-3:3b".to_string())
}

/// Create a live Ollama client
async fn create_live_client() -> Box<dyn LLMClient> {
    let provider = Provider::Ollama {
        base_url: get_ollama_url(),
        model: get_model(),
        params: Default::default(),
    };
    provider
        .create_client()
        .await
        .expect("Failed to create Ollama client")
}

/// Skip test if live tests are not enabled
macro_rules! skip_if_not_live {
    () => {
        if !should_run_live_tests() {
            eprintln!(
                "Skipping live test. Set OLLAMA_LIVE_TESTS=1 to run against real Ollama server."
            );
            return;
        }
    };
}

// ============= Connection Tests =============

#[tokio::test]
#[ignore]
async fn test_live_ollama_connection() {
    skip_if_not_live!();

    let url = get_ollama_url();
    println!("Testing connection to Ollama at: {}", url);

    // Try to connect to the Ollama API
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/api/tags", url))
        .send()
        .await
        .expect("Failed to connect to Ollama");

    assert!(
        response.status().is_success(),
        "Ollama server returned error: {}",
        response.status()
    );

    let body: serde_json::Value = response.json().await.expect("Invalid JSON response");
    println!("Available models: {:?}", body["models"]);
}

// ============= Basic Generation Tests =============

#[tokio::test]
#[ignore]
async fn test_live_ollama_simple_generate() {
    skip_if_not_live!();

    let client = create_live_client().await;
    println!("Using model: {}", client.model_name());

    let response = client
        .generate("Say 'Hello, World!' and nothing else.")
        .await
        .expect("Generation failed");

    println!("Response: {}", response);
    assert!(!response.is_empty(), "Response should not be empty");
}

#[tokio::test]
#[ignore]
async fn test_live_ollama_generate_with_system() {
    skip_if_not_live!();

    let client = create_live_client().await;

    let response = client
        .generate_with_system(
            "You are a helpful assistant that responds in exactly 3 words.",
            "What is the capital of France?",
        )
        .await
        .expect("Generation failed");

    println!("Response: {}", response);
    assert!(!response.is_empty(), "Response should not be empty");
}

#[tokio::test]
#[ignore]
async fn test_live_ollama_generate_with_history() {
    skip_if_not_live!();

    let client = create_live_client().await;

    let messages = vec![
        (
            "system".to_string(),
            "You are a math tutor. Be concise.".to_string(),
        ),
        ("user".to_string(), "What is 2 + 2?".to_string()),
        ("assistant".to_string(), "2 + 2 equals 4.".to_string()),
        ("user".to_string(), "What about 3 + 3?".to_string()),
    ];

    let response = client
        .generate_with_history(&messages)
        .await
        .expect("Generation failed");

    println!("Response: {}", response);
    assert!(!response.is_empty(), "Response should not be empty");
    // The response should mention 6 since we asked about 3+3
}

// ============= Streaming Tests =============

#[tokio::test]
#[ignore]
async fn test_live_ollama_streaming() {
    skip_if_not_live!();

    let client = create_live_client().await;

    let mut stream = client
        .stream("Count from 1 to 5, one number per line.")
        .await
        .expect("Failed to start stream");

    let mut chunks = Vec::new();
    let mut full_response = String::new();

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                print!("{}", chunk); // Print chunks as they arrive
                chunks.push(chunk.clone());
                full_response.push_str(&chunk);
            }
            Err(e) => {
                eprintln!("Stream error: {:?}", e);
                break;
            }
        }
    }
    println!(); // Newline after streaming

    println!("Received {} chunks", chunks.len());
    println!("Full response: {}", full_response);

    assert!(!full_response.is_empty(), "Should receive some response");
    assert!(chunks.len() > 1, "Should receive multiple chunks");
}

// ============= Tool Calling Tests =============

#[tokio::test]
#[ignore]
async fn test_live_ollama_generate_with_tools() {
    skip_if_not_live!();

    let client = create_live_client().await;

    let tools = vec![ares::types::ToolDefinition {
        name: "calculator".to_string(),
        description: "Performs basic arithmetic operations".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["add", "subtract", "multiply", "divide"],
                    "description": "The arithmetic operation to perform"
                },
                "a": {
                    "type": "number",
                    "description": "First operand"
                },
                "b": {
                    "type": "number",
                    "description": "Second operand"
                }
            },
            "required": ["operation", "a", "b"]
        }),
    }];

    let response = client
        .generate_with_tools(
            "What is 15 multiplied by 7? Use the calculator tool.",
            &tools,
        )
        .await
        .expect("Generation failed");

    println!("Content: {}", response.content);
    println!("Finish reason: {}", response.finish_reason);
    println!("Tool calls: {:?}", response.tool_calls);

    // Note: Not all models support tool calling, so we just verify we get a response
    // The finish_reason might be "stop" if the model doesn't support tools
}

// ============= Error Handling Tests =============

#[tokio::test]
#[ignore]
async fn test_live_ollama_invalid_model() {
    skip_if_not_live!();

    let provider = Provider::Ollama {
        base_url: get_ollama_url(),
        model: "nonexistent-model-that-does-not-exist-12345".to_string(),
        params: Default::default(),
    };

    let client = provider
        .create_client()
        .await
        .expect("Client creation should succeed");

    // The error should occur when we try to generate
    let result = client.generate("Hello").await;
    println!("Result with invalid model: {:?}", result);

    // Should get an error about the model not being found
    assert!(result.is_err(), "Should fail with invalid model");
}

// ============= Performance / Load Tests =============

#[tokio::test]
#[ignore]
async fn test_live_ollama_multiple_sequential_requests() {
    skip_if_not_live!();

    let client = create_live_client().await;

    let prompts = [
        "What is 1+1? Answer with just the number.",
        "What is 2+2? Answer with just the number.",
        "What is 3+3? Answer with just the number.",
    ];

    for (i, prompt) in prompts.iter().enumerate() {
        let start = std::time::Instant::now();
        let response = client.generate(prompt).await.expect("Generation failed");
        let elapsed = start.elapsed();

        println!(
            "Request {}: {} (took {:?})",
            i + 1,
            response.trim(),
            elapsed
        );
    }
}

#[tokio::test]
#[ignore]
async fn test_live_ollama_concurrent_requests() {
    skip_if_not_live!();

    let mut handles = vec![];

    for i in 0..3 {
        let handle = tokio::spawn(async move {
            let client = create_live_client().await;
            let prompt = format!("What is {}+{}? Answer with just the number.", i, i);
            let start = std::time::Instant::now();
            let result = client.generate(&prompt).await;
            let elapsed = start.elapsed();
            (i, result, elapsed)
        });
        handles.push(handle);
    }

    for handle in handles {
        let (i, result, elapsed) = handle.await.expect("Task panicked");
        match result {
            Ok(response) => {
                println!("Request {}: {} (took {:?})", i, response.trim(), elapsed);
            }
            Err(e) => {
                println!("Request {} failed: {:?} (took {:?})", i, e, elapsed);
            }
        }
    }
}

// ============= Model Info Tests =============

#[tokio::test]
#[ignore]
async fn test_live_ollama_list_models() {
    skip_if_not_live!();

    let url = get_ollama_url();
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/api/tags", url))
        .send()
        .await
        .expect("Failed to list models");

    let body: serde_json::Value = response.json().await.expect("Invalid JSON");
    let models = body["models"].as_array().expect("Expected models array");

    println!("Available models ({}):", models.len());
    for model in models {
        println!("  - {} ({} bytes)", model["name"], model["size"]);
    }

    assert!(!models.is_empty(), "Should have at least one model");
}
