//! Unit tests for LLM client implementations
//!
//! These tests verify the LLM client trait and provider implementations.

use ares::llm::{LLMClientFactory, Provider};
use ares::types::ToolDefinition;
use serde_json::json;

#[test]
fn test_provider_enum_variants() {
    // Test OpenAI provider creation
    let openai_provider = Provider::OpenAI {
        api_key: "test-key".to_string(),
        api_base: "https://api.openai.com/v1".to_string(),
        model: "gpt-4".to_string(),
    };

    match openai_provider {
        Provider::OpenAI {
            api_key,
            api_base,
            model,
        } => {
            assert_eq!(api_key, "test-key");
            assert_eq!(api_base, "https://api.openai.com/v1");
            assert_eq!(model, "gpt-4");
        }
        _ => panic!("Expected OpenAI provider"),
    }

    // Test Ollama provider creation
    let ollama_provider = Provider::Ollama {
        base_url: "http://localhost:11434".to_string(),
        model: "llama2".to_string(),
    };

    match ollama_provider {
        Provider::Ollama { base_url, model } => {
            assert_eq!(base_url, "http://localhost:11434");
            assert_eq!(model, "llama2");
        }
        _ => panic!("Expected Ollama provider"),
    }

    // Test LlamaCpp provider creation
    let llamacpp_provider = Provider::LlamaCpp {
        model_path: "/path/to/model.gguf".to_string(),
    };

    match llamacpp_provider {
        Provider::LlamaCpp { model_path } => {
            assert_eq!(model_path, "/path/to/model.gguf");
        }
        _ => panic!("Expected LlamaCpp provider"),
    }
}

#[test]
fn test_llm_client_factory_creation() {
    let provider = Provider::Ollama {
        base_url: "http://localhost:11434".to_string(),
        model: "llama2".to_string(),
    };

    let _factory = LLMClientFactory::new(provider);
    // Factory should be created successfully
    // Actual client creation would require a running Ollama instance
}

#[test]
fn test_tool_definition_structure() {
    let tool = ToolDefinition {
        name: "calculator".to_string(),
        description: "Performs basic arithmetic operations".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["add", "subtract", "multiply", "divide"]
                },
                "a": {
                    "type": "number"
                },
                "b": {
                    "type": "number"
                }
            },
            "required": ["operation", "a", "b"]
        }),
    };

    assert_eq!(tool.name, "calculator");
    assert!(!tool.description.is_empty());
    assert!(tool.parameters.is_object());
}

#[test]
fn test_provider_anthropic_variant() {
    let anthropic_provider = Provider::Anthropic {
        api_key: "test-anthropic-key".to_string(),
        model: "claude-3-opus".to_string(),
    };

    match anthropic_provider {
        Provider::Anthropic { api_key, model } => {
            assert_eq!(api_key, "test-anthropic-key");
            assert_eq!(model, "claude-3-opus");
        }
        _ => panic!("Expected Anthropic provider"),
    }
}

#[test]
fn test_provider_clone() {
    let provider = Provider::Ollama {
        base_url: "http://localhost:11434".to_string(),
        model: "llama2".to_string(),
    };

    let cloned = provider.clone();

    match cloned {
        Provider::Ollama { base_url, model } => {
            assert_eq!(base_url, "http://localhost:11434");
            assert_eq!(model, "llama2");
        }
        _ => panic!("Clone failed"),
    }
}
