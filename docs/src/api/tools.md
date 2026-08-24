# Tools

ARES provides a type-safe tool framework with automatic schema generation.

## Built-in tools

| Tool | Description | Feature |
|------|-------------|---------|
| Calculator | Mathematical expression evaluation | default |
| Web Search | Search via [Daedra](https://github.com/dirmacs/daedra) | `search-tools` |
| Web Scrape | Fetch URL and extract readable text content | `search-tools` |

## Tool trait

Implement the `Tool` trait to create a custom tool:

```rust
use ares_tools::Tool;
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

    async fn execute(&self, args: Value) -> ares_types::Result<Value> {
        let input = args["input"].as_str().unwrap_or("");
        Ok(serde_json::json!({ "result": format!("Processed: {}", input) }))
    }
}
```

## Tool registry

```rust
use ares_tools::tool_service::Tools;
use std::sync::Arc;

// Build the Tools service from an explicit tool set
let registry = Tools::from_static([
    Arc::new(Calculator) as Arc<dyn Tool>,
]);

// Get tool definitions for LLM function calling
let ctx = cordis::Context::new_root();
let definitions = registry.list(&ctx);

// Resolve one tool by name
let tool = registry.resolve(&ctx, "calculator");

// Execute a tool by name through the Cordis context
let result = registry.execute(&ctx, "calculator", serde_json::json!({"input": "1 + 1"})).await?;
```

## Tool configuration

Tools read per-tool configuration (enablement and timeouts) from the `ToolConfig` entries in the server configuration:

```toml
[tools.calculator]
timeout_secs = 30
```

## ToolCoordinator

The `ToolCoordinator` in `ares_llm` manages multi-turn conversations with tool calls on any LLM provider:

```rust
use ares_llm::{ToolCoordinator, ToolCallingConfig};

let coordinator = ToolCoordinator::new(client, registry, ToolCallingConfig::default());

// Execute a conversation with automatic tool calling
let result = coordinator.execute(
    Some("You are a helpful assistant."),
    "What is 25 * 4 + 100?",
    &ctx,
).await?;

println!("Response: {}", result.content);
println!("Tool calls: {}", result.tool_calls.len());
```

## Per-Agent tool filtering

You restrict agents to specific tools in the TOON configuration of the agent:

```toon
[agent.math-helper]
tools = ["calculator"]
# This agent can ONLY use the calculator
```

## MCP Bridge

ARES bridges MCP servers into the tool ecosystem. See [MCP Integration](./mcp.md).
