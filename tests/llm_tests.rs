use ares_llm::*;
use ares_types::types::{ToolCall, ToolDefinition};
use futures::StreamExt;

// Import common test utilities
mod common;
use common::mocks::MockLLMClient;

// ============= Basic LLM Client Tests =============

#[tokio::test]
async fn test_mock_llm_client_generate() {
    let client = MockLLMClient::new("Hello, world!");
    let result = client.generate("test prompt").await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "Hello, world!");
}

#[tokio::test]
async fn test_mock_llm_client_generate_with_system() {
    let client = MockLLMClient::new("System response");
    let result = client
        .generate_with_system("You are a helpful assistant", "Hello")
        .await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "System response");
}

#[tokio::test]
async fn test_mock_llm_client_generate_with_history() {
    let client = MockLLMClient::new("History response");
    let messages = vec![
        ("user".to_string(), "Hello".to_string()),
        ("assistant".to_string(), "Hi there!".to_string()),
        ("user".to_string(), "How are you?".to_string()),
    ];
    let result = client.generate_with_history(&messages).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().content, "History response");
}

#[tokio::test]
async fn test_mock_llm_client_generate_with_tools_no_calls() {
    let client = MockLLMClient::new("Tool response");
    let tools: Vec<ToolDefinition> = vec![ToolDefinition {
        name: "calculator".to_string(),
        description: "Performs arithmetic".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {"type": "string"},
                "a": {"type": "number"},
                "b": {"type": "number"}
            },
            "required": ["operation", "a", "b"]
        }),
    }];

    let result = client.generate_with_tools("Calculate 2+2", &tools).await;
    assert!(result.is_ok());
    let response = result.unwrap();
    assert_eq!(response.content, "Tool response");
    assert_eq!(response.finish_reason, "stop");
    assert!(response.tool_calls.is_empty());
}

#[tokio::test]
async fn test_mock_llm_client_generate_with_tools_with_calls() {
    let tool_calls = vec![ToolCall {
        id: "call-1".to_string(),
        name: "calculator".to_string(),
        arguments: serde_json::json!({"operation": "add", "a": 2, "b": 2}),
    }];

    let client = MockLLMClient::with_tool_calls("I need to calculate", tool_calls);
    let tools: Vec<ToolDefinition> = vec![ToolDefinition {
        name: "calculator".to_string(),
        description: "Performs arithmetic".to_string(),
        parameters: serde_json::json!({}),
    }];

    let result = client.generate_with_tools("Calculate 2+2", &tools).await;
    assert!(result.is_ok());
    let response = result.unwrap();
    assert_eq!(response.finish_reason, "tool_calls");
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name, "calculator");
}

#[tokio::test]
async fn test_mock_llm_client_model_name() {
    let client = MockLLMClient::new("test");
    assert_eq!(client.model_name(), "mock-model");
}

#[tokio::test]
async fn test_mock_llm_client_streaming() {
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
async fn test_mock_llm_client_streaming_with_system() {
    let client = MockLLMClient::new("System streaming response");
    let result = client.stream_with_system("You are helpful", "test").await;
    assert!(result.is_ok());

    let mut stream = result.unwrap();
    let mut collected = String::new();

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => collected.push_str(&chunk),
            Err(_) => break,
        }
    }

    assert_eq!(collected, "System streaming response");
}

#[tokio::test]
async fn test_mock_llm_client_streaming_with_history() {
    let client = MockLLMClient::new("History streaming response");
    let history = vec![
        ("user".to_string(), "Hello".to_string()),
        ("assistant".to_string(), "Hi!".to_string()),
    ];
    let result = client.stream_with_history(&history).await;
    assert!(result.is_ok());

    let mut stream = result.unwrap();
    let mut collected = String::new();

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => collected.push_str(&chunk),
            Err(_) => break,
        }
    }

    assert_eq!(collected, "History streaming response");
}

// ============= LLM Response Tests =============

#[test]
fn test_llm_response_struct() {
    let response = LLMResponse {
        content: "Test content".to_string(),
        tool_calls: vec![],
        finish_reason: "stop".to_string(),
        usage: None,
        reasoning_content: None,
        response_id: None,
    };

    assert_eq!(response.content, "Test content");
    assert!(response.tool_calls.is_empty());
    assert_eq!(response.finish_reason, "stop");
}

#[test]
fn test_llm_response_with_tool_calls() {
    let tool_calls = vec![
        ToolCall {
            id: "1".to_string(),
            name: "func1".to_string(),
            arguments: serde_json::json!({"arg": "value"}),
        },
        ToolCall {
            id: "2".to_string(),
            name: "func2".to_string(),
            arguments: serde_json::json!({"num": 42}),
        },
    ];

    let response = LLMResponse {
        content: "".to_string(),
        tool_calls: tool_calls.clone(),
        finish_reason: "tool_calls".to_string(),
        usage: None,
        reasoning_content: None,
        response_id: None,
    };

    assert_eq!(response.tool_calls.len(), 2);
    assert_eq!(response.tool_calls[0].name, "func1");
    assert_eq!(response.tool_calls[1].arguments["num"], 42);
}

// ============= Provider Tests =============

fn ollama_provider(base_url: &str, model: &str) -> Provider {
    Provider::Genai(GenaiProvider {
        kind: AdapterKind::Ollama,
        api_key: None,
        endpoint: Some(base_url.to_string()),
        model: model.to_string(),
        params: Default::default(),
        headers: Default::default(),
        region: None,
        vertex_project: None,
        vertex_location: None,
        custom_index: None,
    })
}

#[test]
fn test_provider_from_env_no_config() {
    unsafe {
        std::env::remove_var("LLAMACPP_MODEL_PATH");
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("NVIDIA_API_KEY");
        std::env::remove_var("AZURE_FOUNDRY_API_KEY");
        std::env::remove_var("AWS_BEARER_TOKEN_BEDROCK");
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("GEMINI_API_KEY");
        std::env::remove_var("OLLAMA_URL");
        std::env::remove_var("OLLAMA_BASE_URL");
        std::env::remove_var("OLLAMA_MODEL");
    }

    let result = Provider::from_env().expect("ollama localhost fallback");
    assert_eq!(result.name(), "ollama");
    assert!(!result.requires_api_key());
    assert!(result.is_local());
}

// ============= Tool Definition Tests =============

#[test]
fn test_tool_definition_serialization() {
    let tool = ToolDefinition {
        name: "search".to_string(),
        description: "Search the web".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search query"}
            },
            "required": ["query"]
        }),
    };

    assert_eq!(tool.name, "search");
    assert_eq!(tool.description, "Search the web");
    assert!(tool.parameters["properties"]["query"].is_object());
}

#[test]
fn test_tool_call_structure() {
    let call = ToolCall {
        id: "unique-id-123".to_string(),
        name: "get_weather".to_string(),
        arguments: serde_json::json!({
            "city": "London",
            "units": "celsius"
        }),
    };

    assert_eq!(call.id, "unique-id-123");
    assert_eq!(call.name, "get_weather");
    assert_eq!(call.arguments["city"], "London");
    assert_eq!(call.arguments["units"], "celsius");
}

// ============= LLMClientFactory Tests =============

#[test]
fn test_llm_client_factory_creation() {
    let factory =
        LLMClientFactory::new(ollama_provider("http://localhost:11434", "ministral-3:3b"));
    assert_eq!(factory.default_provider().name(), "ollama");
    assert!(std::mem::size_of_val(&factory) > 0);
}

#[test]
fn test_provider_from_config_openai_mapping() {
    std::env::set_var("TEST_ARES_LLM_OPENAI_KEY", "sk-test");
    let config = ProviderConfig::OpenAI {
        api_key_env: "TEST_ARES_LLM_OPENAI_KEY".to_string(),
        api_base: "https://api.openai.com/v1".to_string(),
        default_model: "gpt-4".to_string(),
    };
    let provider = Provider::from_config(&config, None).expect("from_config openai");
    assert_eq!(provider.name(), "openai");
    std::env::remove_var("TEST_ARES_LLM_OPENAI_KEY");
}

// ============= Multi-Message Conversation Tests =============

#[tokio::test]
async fn test_multi_turn_conversation() {
    let client = MockLLMClient::new("Final response after history");

    // Simulate a multi-turn conversation
    let history = vec![
        (
            "system".to_string(),
            "You are a helpful assistant.".to_string(),
        ),
        ("user".to_string(), "What is 2+2?".to_string()),
        ("assistant".to_string(), "2+2 equals 4.".to_string()),
        ("user".to_string(), "What about 3+3?".to_string()),
    ];

    let result = client.generate_with_history(&history).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_empty_history() {
    let client = MockLLMClient::new("Response to empty history");
    let history: Vec<(String, String)> = vec![];

    let result = client.generate_with_history(&history).await;
    assert!(result.is_ok());
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
    let long_prompt = "test ".repeat(1000);
    let result = client.generate(&long_prompt).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_unicode_prompt() {
    let client = MockLLMClient::new("Response with unicode: 你好世界 🌍");
    let result = client.generate("Hello in Chinese: 你好").await;
    assert!(result.is_ok());
    assert!(result.unwrap().contains("你好世界"));
}

#[tokio::test]
async fn test_special_characters_in_prompt() {
    let client = MockLLMClient::new("Response with special chars");
    let prompt = r#"Test with "quotes", 'apostrophes', \backslash, and {braces}"#;
    let result = client.generate(prompt).await;
    assert!(result.is_ok());
}

// ============= Tool Call Argument Tests =============

#[test]
fn test_tool_call_complex_arguments() {
    let call = ToolCall {
        id: "complex-call".to_string(),
        name: "complex_tool".to_string(),
        arguments: serde_json::json!({
            "string_arg": "hello",
            "number_arg": 42,
            "float_arg": 2.75,
            "bool_arg": true,
            "null_arg": null,
            "array_arg": [1, 2, 3],
            "object_arg": {"nested": "value"}
        }),
    };

    assert_eq!(call.arguments["string_arg"], "hello");
    assert_eq!(call.arguments["number_arg"], 42);
    assert!(call.arguments["bool_arg"].as_bool().unwrap());
    assert!(call.arguments["null_arg"].is_null());
    assert_eq!(call.arguments["array_arg"].as_array().unwrap().len(), 3);
    assert_eq!(call.arguments["object_arg"]["nested"], "value");
}

// ============= Multiple Tool Calls Tests =============

#[tokio::test]
async fn test_multiple_tool_calls_in_single_response() {
    let tool_calls = vec![
        ToolCall {
            id: "call-1".to_string(),
            name: "get_weather".to_string(),
            arguments: serde_json::json!({"city": "London"}),
        },
        ToolCall {
            id: "call-2".to_string(),
            name: "get_time".to_string(),
            arguments: serde_json::json!({"timezone": "UTC"}),
        },
        ToolCall {
            id: "call-3".to_string(),
            name: "search".to_string(),
            arguments: serde_json::json!({"query": "news"}),
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
