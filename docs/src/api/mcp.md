# MCP Integration

ARES integrates with [Model Context Protocol](https://modelcontextprotocol.io) servers, allowing agents to use external tools as first-class capabilities.

## Feature flag

```toml
[dependencies]
ares-server = { version = "0.7", features = ["mcp"] }
```

MCP is included in the default feature set.

## Configuration

MCP servers are configured via `.toon` files in your config directory. Each server gets its own TOON configuration.

## How it works

1. ARES discovers MCP server configs from the config directory
2. `McpRegistry::from_dir()` loads and connects to configured servers
3. Each server provides an `McpClient` for tool invocation
4. Agents access MCP tools through the registry

## Architecture

```
Agent Request → McpRegistry → get_client("eruka") → McpClient → MCP Server
                                                                      ↓
Agent Response ← Tool Result ←────────────────────────────────────────┘
```

## Library usage

```rust
use ares::mcp::McpRegistry;

// Load MCP servers from config directory
let registry = McpRegistry::from_dir("config/mcp")?;

// List connected servers
let names = registry.client_names();

// Get a specific client
if let Some(client) = registry.get_client("eruka") {
    // Use the client to call MCP tools
}

// Convenience method for Eruka specifically
if let Some(eruka) = registry.eruka() {
    // Direct access to Eruka MCP client
}
```

## Per-Agent MCP Access

Agents can be configured with specific MCP server access via TOON configuration:

```toon
[agent.researcher]
mcp_servers = ["eruka", "search"]
```
