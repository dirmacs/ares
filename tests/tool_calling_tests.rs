//! Integration tests for tool calling functionality
//!
//! These tests verify the end-to-end tool calling workflow:
//! - Tool registry management
//! - LLM tool calling (Ollama and OpenAI)
//! - Daedra integration for web search and page fetching

use ares::tools::{ToolRegistry, Tool};
use ares::types::ToolDefinition;
use serde_json::json;

#[test]
fn test_tool_registry_initialization() {
    let registry = ToolRegistry::with_default_tools();
    let tools = registry.tool_names();
    
    assert!(tools.len() >= 3, "Should have at least 3 default tools");
    assert!(registry.has_tool("web_search"), "Should have web_search tool");
    assert!(registry.has_tool("fetch_page"), "Should have fetch_page tool");
    assert!(registry.has_tool("calculator"), "Should have calculator tool");
}

#[test]
fn test_tool_definitions_schema() {
    let registry = ToolRegistry::with_default_tools();
    let definitions = registry.get_tool_definitions();
    
    // Verify we have all expected tools
    let tool_names: Vec<String> = definitions.iter().map(|d| d.name.clone()).collect();
    assert!(tool_names.contains(&"web_search".to_string()));
    assert!(tool_names.contains(&"fetch_page".to_string()));
    assert!(tool_names.contains(&"calculator".to_string()));
    
    // Verify each tool has proper schema
    for def in &definitions {
        assert!(!def.name.is_empty(), "Tool name should not be empty");
        assert!(!def.description.is_empty(), "Tool description should not be empty");
        assert!(def.parameters.is_object(), "Tool parameters should be an object");
        
        // Check for OpenAI function calling compatibility
        let params = &def.parameters;
        assert_eq!(params["type"], "object", "Parameters type should be 'object'");
        assert!(params.get("properties").is_some(), "Should have properties field");
    }
}

#[tokio::test]
async fn test_calculator_tool_execution() {
    let registry = ToolRegistry::with_default_tools();
    
    // Test addition
    let add_args = json!({
        "operation": "add",
        "a": 10.0,
        "b": 5.0
    });
    let result = registry.execute("calculator", add_args).await.unwrap();
    assert_eq!(result["result"], 15.0);
    
    // Test multiplication
    let mul_args = json!({
        "operation": "multiply",
        "a": 4.0,
        "b": 3.0
    });
    let result = registry.execute("calculator", mul_args).await.unwrap();
    assert_eq!(result["result"], 12.0);
    
    // Test division
    let div_args = json!({
        "operation": "divide",
        "a": 20.0,
        "b": 4.0
    });
    let result = registry.execute("calculator", div_args).await.unwrap();
    assert_eq!(result["result"], 5.0);
}

#[tokio::test]
async fn test_search_tool_schema() {
    let registry = ToolRegistry::with_default_tools();
    let definitions = registry.get_tool_definitions();
    
    let search_tool = definitions
        .iter()
        .find(|d| d.name == "web_search")
        .expect("Should have web_search tool");
    
    // Verify schema structure
    assert_eq!(search_tool.name, "web_search");
    assert!(search_tool.description.contains("search") || search_tool.description.contains("web"));
    
    let params = &search_tool.parameters;
    assert_eq!(params["type"], "object");
    assert!(params["properties"]["query"].is_object());
    assert!(params["required"].as_array().unwrap().contains(&json!("query")));
}

#[tokio::test]
async fn test_fetch_page_tool_schema() {
    let registry = ToolRegistry::with_default_tools();
    let definitions = registry.get_tool_definitions();
    
    let fetch_tool = definitions
        .iter()
        .find(|d| d.name == "fetch_page")
        .expect("Should have fetch_page tool");
    
    // Verify schema structure
    assert_eq!(fetch_tool.name, "fetch_page");
    assert!(fetch_tool.description.contains("fetch") || fetch_tool.description.contains("page"));
    
    let params = &fetch_tool.parameters;
    assert_eq!(params["type"], "object");
    assert!(params["properties"]["url"].is_object());
    assert!(params["required"].as_array().unwrap().contains(&json!("url")));
}

#[test]
fn test_tool_definition_for_llm() {
    let registry = ToolRegistry::with_default_tools();
    let definitions = registry.get_tool_definitions();
    
    // Verify that tool definitions can be serialized for LLM APIs
    for def in &definitions {
        let serialized = serde_json::to_string(&def).unwrap();
        assert!(!serialized.is_empty());
        
        // Verify it can be deserialized back
        let deserialized: ToolDefinition = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.name, def.name);
        assert_eq!(deserialized.description, def.description);
    }
}

#[tokio::test]
async fn test_invalid_tool_name() {
    let registry = ToolRegistry::with_default_tools();
    
    let result = registry.execute("nonexistent_tool", json!({})).await;
    assert!(result.is_err());
    
    let err = result.unwrap_err();
    assert!(err.to_string().contains("not found") || err.to_string().contains("Tool"));
}

#[tokio::test]
async fn test_calculator_invalid_operation() {
    let registry = ToolRegistry::with_default_tools();
    
    let args = json!({
        "operation": "invalid",
        "a": 5.0,
        "b": 3.0
    });
    
    let result = registry.execute("calculator", args).await;
    // Should still execute but return 0.0 for unknown operations
    assert!(result.is_ok());
}

#[test]
fn test_custom_tool_registration() {
    use async_trait::async_trait;
    use std::sync::Arc;
    
    // Define a simple custom tool
    struct CustomTool;
    
    #[async_trait]
    impl Tool for CustomTool {
        fn name(&self) -> &str {
            "custom_tool"
        }
        
        fn description(&self) -> &str {
            "A custom test tool"
        }
        
        fn parameters_schema(&self) -> serde_json::Value {
            json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "Input parameter"
                    }
                },
                "required": ["input"]
            })
        }
        
        async fn execute(&self, args: serde_json::Value) -> ares::types::Result<serde_json::Value> {
            let input = args["input"].as_str().unwrap_or("default");
            Ok(json!({ "output": input.to_uppercase() }))
        }
    }
    
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CustomTool));
    
    assert!(registry.has_tool("custom_tool"));
    assert_eq!(registry.tool_names().len(), 1);
}

#[tokio::test]
async fn test_custom_tool_execution() {
    use async_trait::async_trait;
    use std::sync::Arc;
    
    struct EchoTool;
    
    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        
        fn description(&self) -> &str {
            "Echoes back the input"
        }
        
        fn parameters_schema(&self) -> serde_json::Value {
            json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string"
                    }
                },
                "required": ["message"]
            })
        }
        
        async fn execute(&self, args: serde_json::Value) -> ares::types::Result<serde_json::Value> {
            Ok(json!({ "echo": args["message"] }))
        }
    }
    
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));
    
    let result = registry.execute("echo", json!({ "message": "Hello" })).await.unwrap();
    assert_eq!(result["echo"], "Hello");
}
