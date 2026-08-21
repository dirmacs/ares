# Tools

ARES provides a type-safe tool calling framework with automatic schema generation.

## Built-in tools

| Tool | Description | Feature |
|------|-------------|---------|
| Calculator | Mathematical expression evaluation | default |
| Web Search | Search via [Daedra](https://github.com/dirmacs/daedra) | `search-tools` |
| Web Scrape | Fetch URL and extract readable text content | `search-tools` |

## Tool trait

Implement `Tool` to create custom tools:

```rust
use ares::tools::registry::Tool;
use async_trait::async_trait;
use serde_json::Value;

struct MyTool;

#[async_trait]
impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }

    fn description(&self) -> &str { "Does something useful" }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "input": { "type": "string" }
            }
        })
    }

    async fn execute(&self, args: Value) -> ares::Result<Value> {
        let input = args["input"].as_str().unwrap_or("");
        Ok(serde_json::json!({ "result": format!("Processed: {}", input) }))
    }
}
```

## Tool registry

```rust
use ares::tools::ToolRegistry;
use std::sync::Arc;

// Create empty registry
let mut registry = ToolRegistry::new();

// Or create from config (auto-registers configured tools)
let mut registry = ToolRegistry::with_config(&config);

// Register a custom tool
registry.register(Arc::new(MyTool));

// Get tool definitions for LLM function calling
let definitions = registry.get_tool_definitions();

// Get definitions for specific tools only
let subset = registry.get_tool_definitions_for(&["calculator", "my_tool"]);

// Execute a tool by name
let result = registry.execute("my_tool", serde_json::json!({"input": "hello"})).await?;

// Check tool availability
assert!(registry.has_tool("calculator"));
```

## Tool configuration

Tools support per-tool configuration (enabled/disabled, timeouts):

```rust
// Check if a tool is enabled
registry.is_enabled("web_search");

// Get tool timeout
let timeout_secs = registry.get_timeout("web_search");
```

## ToolCoordinator

The `ToolCoordinator` (in `ares::llm`) handles multi-turn tool calling conversations with any LLM provider:

```rust
use ares::llm::{ToolCoordinator, ToolCallingConfig};

let coordinator = ToolCoordinator::new(client, registry, ToolCallingConfig::default());

// Execute a conversation with automatic tool calling
let result = coordinator.execute(
    Some("You are a helpful assistant."),
    "What is 25 * 4 + 100?"
).await?;

println!("Response: {}", result.content);
println!("Tool calls: {}", result.tool_calls.len());
```

## Per-Agent tool filtering

Agents can be restricted to specific tools via TOON configuration:

```toon
[agent.math-helper]
tools = ["calculator"]
# This agent can ONLY use the calculator
```

## MCP Bridge

MCP servers are bridged into the tool ecosystem. See [MCP Integration](./mcp.md).
