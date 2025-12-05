# Tool Calling Examples

This document provides examples of using the tool calling functionality in ARES with different LLM providers.

## Table of Contents

- [Ollama Tool Calling](#ollama-tool-calling)
- [OpenAI Tool Calling](#openai-tool-calling)
- [Tool Registry](#tool-registry)
- [Custom Tools](#custom-tools)
- [Daedra Integration](#daedra-integration)

## Ollama Tool Calling

Ollama supports tool calling for compatible models (llama3.1+, mistral-nemo, etc.). Here's how to use it:

### Basic Example

```rust
use ares::llm::{LLMClient, Provider};
use ares::tools::ToolRegistry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create Ollama client
    let provider = Provider::Ollama {
        base_url: "http://localhost:11434".to_string(),
        model: "llama3.1".to_string(), // Must be a model that supports tool calling
    };
    
    let client = provider.create_client().await?;
    
    // Create tool registry with default tools (web_search, fetch_page, calculator)
    let registry = ToolRegistry::with_default_tools();
    let tools = registry.get_tool_definitions();
    
    // Generate with tool calling
    let response = client.generate_with_tools(
        "What's 42 plus 17? Use the calculator.",
        &tools
    ).await?;
    
    println!("Response: {}", response.content);
    
    // Handle tool calls if any
    for tool_call in &response.tool_calls {
        println!("Tool called: {}", tool_call.name);
        
        // Execute the tool
        let result = registry.execute(&tool_call.name, tool_call.arguments.clone()).await?;
        println!("Tool result: {:?}", result);
    }
    
    Ok(())
}
```

### Web Search with Ollama

```rust
use ares::llm::{LLMClient, Provider};
use ares::tools::ToolRegistry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = Provider::Ollama {
        base_url: "http://localhost:11434".to_string(),
        model: "llama3.1".to_string(),
    };
    
    let client = provider.create_client().await?;
    let registry = ToolRegistry::with_default_tools();
    let tools = registry.get_tool_definitions();
    
    // Ask the model to search for information
    let response = client.generate_with_tools(
        "Search the web for information about Rust programming language",
        &tools
    ).await?;
    
    // Execute any tool calls
    for tool_call in &response.tool_calls {
        if tool_call.name == "web_search" {
            let result = registry.execute(&tool_call.name, tool_call.arguments.clone()).await?;
            println!("Search results: {:?}", result);
        }
    }
    
    Ok(())
}
```

## OpenAI Tool Calling

OpenAI models (GPT-4, GPT-3.5-turbo) have full support for function calling:

```rust
use ares::llm::{LLMClient, Provider};
use ares::tools::ToolRegistry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = Provider::OpenAI {
        api_key: std::env::var("OPENAI_API_KEY")?,
        api_base: "https://api.openai.com/v1".to_string(),
        model: "gpt-4o-mini".to_string(),
    };
    
    let client = provider.create_client().await?;
    let registry = ToolRegistry::with_default_tools();
    let tools = registry.get_tool_definitions();
    
    let response = client.generate_with_tools(
        "Calculate 123 * 456 and then search for information about the number",
        &tools
    ).await?;
    
    // OpenAI may make multiple tool calls in sequence
    for tool_call in &response.tool_calls {
        println!("Executing tool: {}", tool_call.name);
        let result = registry.execute(&tool_call.name, tool_call.arguments.clone()).await?;
        println!("Result: {:?}", result);
    }
    
    Ok(())
}
```

## Tool Registry

The tool registry manages all available tools and provides a unified interface for tool execution.

### Creating a Registry with Default Tools

```rust
use ares::tools::ToolRegistry;

// Create registry with default tools (web_search, fetch_page, calculator)
let registry = ToolRegistry::with_default_tools();

// Check what tools are available
for name in registry.tool_names() {
    println!("Available tool: {}", name);
}

// Get tool definitions for LLM
let definitions = registry.get_tool_definitions();
println!("Total tools: {}", definitions.len());
```

### Using Individual Tools

```rust
use ares::tools::ToolRegistry;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry = ToolRegistry::with_default_tools();
    
    // Use calculator
    let result = registry.execute("calculator", json!({
        "operation": "multiply",
        "a": 15.0,
        "b": 7.0
    })).await?;
    
    println!("15 * 7 = {}", result["result"]);
    
    // Use web search (requires internet)
    let search_result = registry.execute("web_search", json!({
        "query": "Rust programming language",
        "num_results": 5
    })).await?;
    
    println!("Search results: {:?}", search_result);
    
    Ok(())
}
```

## Custom Tools

You can create and register custom tools:

```rust
use ares::tools::{Tool, ToolRegistry};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

struct WeatherTool;

#[async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &str {
        "get_weather"
    }
    
    fn description(&self) -> &str {
        "Get the current weather for a location"
    }
    
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "location": {
                    "type": "string",
                    "description": "The city and state, e.g. San Francisco, CA"
                },
                "unit": {
                    "type": "string",
                    "enum": ["celsius", "fahrenheit"],
                    "description": "The temperature unit"
                }
            },
            "required": ["location"]
        })
    }
    
    async fn execute(&self, args: Value) -> ares::types::Result<Value> {
        let location = args["location"].as_str().unwrap_or("Unknown");
        let unit = args["unit"].as_str().unwrap_or("celsius");
        
        // In a real implementation, call a weather API
        Ok(json!({
            "location": location,
            "temperature": 22,
            "unit": unit,
            "condition": "Sunny"
        }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = ToolRegistry::new();
    
    // Register custom tool
    registry.register(Arc::new(WeatherTool));
    
    // Register default tools too
    registry.register(Arc::new(ares::tools::search::SearchTool::new()));
    
    // Now use with LLM
    let tools = registry.get_tool_definitions();
    println!("Available tools: {:?}", tools.iter().map(|t| &t.name).collect::<Vec<_>>());
    
    Ok(())
}
```

## Daedra Integration

ARES integrates [daedra](https://crates.io/crates/daedra) for web search and page fetching capabilities. These tools work completely locally without requiring API keys.

### Web Search Tool

```rust
use ares::tools::search::SearchTool;
use ares::tools::Tool;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let search_tool = SearchTool::new();
    
    // Search the web using DuckDuckGo
    let result = search_tool.execute(json!({
        "query": "Rust async programming",
        "num_results": 10
    })).await?;
    
    // Results include title, URL, and description
    println!("Search results: {:?}", result);
    
    Ok(())
}
```

### Page Fetch Tool

```rust
use ares::tools::search::FetchPageTool;
use ares::tools::Tool;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fetch_tool = FetchPageTool::new();
    
    // Fetch a web page and convert to markdown
    let result = fetch_tool.execute(json!({
        "url": "https://www.rust-lang.org/",
        "selector": None  // Optional CSS selector for specific content
    })).await?;
    
    println!("Page content: {}", result["content"]);
    println!("Word count: {}", result["word_count"]);
    
    Ok(())
}
```

### Full Example: Research Assistant

Combining all tools to create a research assistant:

```rust
use ares::llm::{LLMClient, Provider};
use ares::tools::ToolRegistry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup Ollama client
    let provider = Provider::Ollama {
        base_url: "http://localhost:11434".to_string(),
        model: "llama3.1".to_string(),
    };
    let client = provider.create_client().await?;
    
    // Setup tools
    let registry = ToolRegistry::with_default_tools();
    let tools = registry.get_tool_definitions();
    
    // Research query
    let query = "Research the history of Rust programming language and calculate how many years ago it was first released (2010)";
    
    // First LLM call with tools
    let mut response = client.generate_with_tools(query, &tools).await?;
    
    // Execute tool calls iteratively
    let mut iterations = 0;
    while !response.tool_calls.is_empty() && iterations < 5 {
        println!("\nIteration {}: Executing {} tools", iterations + 1, response.tool_calls.len());
        
        for tool_call in &response.tool_calls {
            println!("  - Tool: {}", tool_call.name);
            
            let result = registry.execute(&tool_call.name, tool_call.arguments.clone()).await?;
            println!("  - Result: {:?}", result);
            
            // In a full implementation, you'd send the result back to the LLM
            // to continue the conversation
        }
        
        iterations += 1;
        
        // For this example, we break after first iteration
        break;
    }
    
    println!("\nFinal response: {}", response.content);
    
    Ok(())
}
```

## Model Compatibility

### Ollama Models with Tool Calling Support

- ✅ **llama3.1** (all sizes) - Full tool calling support
- ✅ **llama3.2** (3B+) - Full tool calling support  
- ✅ **mistral-nemo** - Full tool calling support
- ✅ **qwen2.5** (7B+) - Full tool calling support
- ❌ **llama2** - No tool calling support (will ignore tools)
- ❌ **mistral** (v0.1-0.2) - No tool calling support

### Checking Model Support

```rust
use ares::llm::{LLMClient, Provider};
use ares::tools::ToolRegistry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = Provider::Ollama {
        base_url: "http://localhost:11434".to_string(),
        model: "llama2".to_string(), // Model without tool calling
    };
    
    let client = provider.create_client().await?;
    let registry = ToolRegistry::with_default_tools();
    let tools = registry.get_tool_definitions();
    
    let response = client.generate_with_tools(
        "Calculate 5 + 3",
        &tools
    ).await?;
    
    if response.tool_calls.is_empty() {
        println!("Model did not use tools - may not support tool calling");
        println!("Response: {}", response.content);
    } else {
        println!("Tool calls detected: {:?}", response.tool_calls.len());
    }
    
    Ok(())
}
```

## Best Practices

1. **Use compatible models**: Always use models that support tool calling (llama3.1+, mistral-nemo) when using the `generate_with_tools` method.

2. **Handle missing tool calls gracefully**: Not all models will use tools even when provided. Check if `tool_calls` is empty and handle accordingly.

3. **Limit iterations**: When implementing agentic loops, always limit the number of tool calling iterations to prevent infinite loops.

4. **Validate tool results**: Always validate and sanitize tool results before passing them back to the LLM.

5. **Local-first**: The default tools (web_search via daedra, calculator) work completely locally without API keys, making ARES truly local-first.

6. **Error handling**: Tool execution can fail (network issues, invalid arguments). Always handle errors gracefully:

```rust
match registry.execute(&tool_call.name, tool_call.arguments.clone()).await {
    Ok(result) => println!("Success: {:?}", result),
    Err(e) => eprintln!("Tool execution failed: {}", e),
}
```

## Troubleshooting

### "Tool not found" error

Make sure the tool is registered in the registry:

```rust
let registry = ToolRegistry::with_default_tools();
assert!(registry.has_tool("web_search"));
```

### Model not making tool calls

1. Verify you're using a compatible model (llama3.1+)
2. Make sure your prompt explicitly mentions using tools
3. Check that tools are properly passed to `generate_with_tools`

### Ollama connection errors

Ensure Ollama is running:

```bash
ollama serve
# In another terminal:
ollama pull llama3.1
```

### DuckDuckGo search rate limiting

The daedra search tool uses DuckDuckGo, which may rate limit requests. If you encounter issues:

1. Add delays between searches
2. Reduce the number of results requested
3. Consider caching search results

## Further Reading

- [Ollama Tool Calling Documentation](https://github.com/ollama/ollama/blob/main/docs/api.md#generate-a-chat-completion)
- [OpenAI Function Calling Guide](https://platform.openai.com/docs/guides/function-calling)
- [Daedra Crate Documentation](https://docs.rs/daedra/)
- [ARES API Documentation](http://localhost:3000/swagger-ui/)
