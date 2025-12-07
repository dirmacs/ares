# MCP (Model Context Protocol) Server

A.R.E.S includes a full MCP server implementation that exposes tools via the Model Context Protocol, enabling integration with AI assistants like Claude Desktop, Zed, and other MCP-compatible clients.

## Features

- **Calculator Tool**: Perform basic arithmetic operations (add, subtract, multiply, divide)
- **Web Search Tool**: Search the web using DuckDuckGo via daedra (no API key required)
- **Server Stats Tool**: Get server statistics and operation counts
- **Echo Tool**: Simple echo for testing connectivity

## Enabling MCP

MCP support is feature-gated. Enable it during compilation:

```bash
cargo build --features mcp
```

Or with other features:

```bash
cargo build --features "mcp,ollama"
```

## Starting the MCP Server

The MCP server runs over stdio (standard input/output) as per the MCP specification:

```rust
use ares::mcp::McpServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    McpServer::start().await?;
    Ok(())
}
```

## Configuring with Claude Desktop

Add the following to your Claude Desktop configuration file:

**macOS**: `~/Library/Application Support/Claude/claude_desktop_config.json`  
**Windows**: `%APPDATA%\Claude\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "ares": {
      "command": "/path/to/ares-mcp-server",
      "args": []
    }
  }
}
```

## Available Tools

### calculator

Perform basic arithmetic operations.

**Parameters:**
- `operation` (string, required): One of "add", "subtract", "multiply", "divide"
- `a` (number, required): First operand
- `b` (number, required): Second operand

**Example:**
```json
{
  "operation": "multiply",
  "a": 6,
  "b": 7
}
```

**Response:**
```json
{
  "operation": "multiply",
  "a": 6,
  "b": 7,
  "result": 42
}
```

### web_search

Search the web for information using DuckDuckGo.

**Parameters:**
- `query` (string, required): The search query
- `max_results` (integer, optional): Maximum results to return (default: 5)

**Example:**
```json
{
  "query": "rust programming language",
  "max_results": 3
}
```

**Response:**
```json
{
  "query": "rust programming language",
  "results": [
    {
      "title": "Rust Programming Language",
      "url": "https://www.rust-lang.org/",
      "snippet": "A language empowering everyone to build reliable and efficient software."
    }
  ],
  "count": 1
}
```

### server_stats

Get statistics about the MCP server.

**Parameters:** None

**Response:**
```json
{
  "server": "ARES MCP Server",
  "version": "0.1.1",
  "operation_count": 42,
  "available_tools": ["calculator", "web_search", "server_stats", "echo"]
}
```

### echo

Echo back a message (useful for testing).

**Parameters:**
- `message` (string, required): The message to echo back

**Example:**
```json
{
  "message": "Hello, MCP!"
}
```

**Response:**
```
Hello, MCP!
```

## Programmatic Usage

You can also use the MCP server programmatically:

```rust
use ares::mcp::McpServer;

// Create server instance
let server = McpServer::new();

// Execute tools directly
let result = server.execute_tool("calculator", Some(args)).await;
```

## Testing

Run MCP-specific tests:

```bash
cargo test --features mcp
```

This runs 14 additional MCP-related tests covering:
- Tool argument parsing
- Calculator operations (add, subtract, multiply, divide)
- Division by zero handling
- Unknown operation handling
- Echo functionality
- Server statistics
- Operation count tracking
- Tool execution via name lookup

## Implementation Details

The MCP server is implemented in `src/mcp/server.rs` using the `rmcp` crate (Rust MCP SDK). Key components:

- **McpServer**: Main server struct implementing `ServerHandler`
- **Tool definitions**: JSON Schema-based tool definitions
- **execute_tool**: Unified tool execution by name
- **Operation tracking**: Mutex-protected operation counter

## Protocol Version

The server uses MCP protocol version `2024-11-05` (latest as of implementation).

## Error Handling

Tool execution errors are returned as `CallToolResult::error` with descriptive messages:

- Invalid arguments: "Invalid calculator arguments: ..."
- Division by zero: "Error: Division by zero"
- Unknown operation: "Error: Unknown operation '...'"
- Unknown tool: "Unknown tool: ..."

## See Also

- [Model Context Protocol Specification](https://modelcontextprotocol.io/)
- [rmcp Rust SDK](https://github.com/modelcontextprotocol/rust-sdk)
- [PROJECT_STATUS.md](./PROJECT_STATUS.md) - Overall project status