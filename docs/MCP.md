# MCP (Model Context Protocol) server

A.R.E.S includes an MCP server that exposes tools over the Model Context Protocol. AI assistants can connect to it, for example Claude Desktop and Zed.

## Features

The built-in tools are `ares_list_agents`, `ares_run_agent`, `ares_get_status`, `ares_deploy_agent`, and `ares_get_usage`. They list agents, run agents with a message, check run status, deploy `.toon` configurations, and show usage data.

## Enabling MCP

MCP support is feature-gated. Enable the feature at compile time:

```bash
cargo build --features mcp
```

Combine it with other features as needed:

```bash
cargo build --features "mcp,openai"
```

## Starting the MCP server

The MCP server communicates over stdio, as the MCP specification requires. The binary starts it in MCP mode:

```bash
cargo build --features "mcp,postgres"
ARES_API_KEY=<your-key> ares-server --mcp
```

The server mode requires the `postgres` feature together with `mcp`.

## Configuring with Claude Desktop

Add this entry to your Claude Desktop configuration file:

- macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
- Windows: `%APPDATA%\Claude\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "ares": {
      "command": "/path/to/ares-server",
      "args": ["--mcp"]
    }
  }
}
```

## Available tools

### Calculator

This section describes the legacy tool set from earlier releases. The current server ships the five ARES tools above.

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

### Web_search

This section describes the legacy tool set from earlier releases.

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

### Server_stats

This section describes the legacy tool set from earlier releases.

**Response:**
```json
{
  "server": "ARES MCP Server",
  "version": "0.3.3",
  "operation_count": 42,
  "available_tools": ["calculator", "web_search", "server_stats", "echo"]
}
```

### Echo

This section describes the legacy tool set from earlier releases.

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

## Programmatic usage

You can also start the MCP server from Rust through the public function in the `ares-mcp` crate:

```rust
use ares_mcp::start_mcp_server;
```

`start_mcp_server(tenant_db, pool, ares_api_url, runner)` needs a tenant database, a PostgreSQL pool, the API base URL, and an optional runner.

## Testing

Run the MCP tests with this command:

```bash
cargo test --features mcp
```

The suite covers these areas:
- Tool argument parsing
- Authentication before tool calls
- Unknown operation handling
- The five agent operations: listing, running, status, deployment, and usage
- Extension dispatch
- Tool execution by name lookup

## Implementation details

The MCP server lives in `crates/ares-mcp/src/server.rs` and uses the `rmcp` crate (Rust MCP SDK). Key components:
- `AresMcpServer`: main struct that implements `ServerHandler`
- `builtin_ares_tools`: JSON Schema-based definitions for the five ARES tools
- `execute_tool`: unified tool dispatch by name
- `McpRegistry`: extension registry for external MCP tool servers
- usage recording per operation

## Protocol version

The server speaks MCP protocol version `2024-11-05`.

## Error handling

Tool failures return `CallToolResult` errors with descriptive messages. Examples include invalid arguments and unknown tool names.

## See also

- [Model Context Protocol specification](https://modelcontextprotocol.io/)
- [rmcp Rust SDK](https://github.com/modelcontextprotocol/rust-sdk)
- [PROJECT_STATUS.md](./PROJECT_STATUS.md) — overall project status
