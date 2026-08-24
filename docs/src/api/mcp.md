# MCP Integration

ARES integrates with [Model Context Protocol](https://modelcontextprotocol.io) servers. Agents use external tools from these servers as first-class capabilities.

## Feature flag

```toml
[dependencies]
ares-server = { version = "0.9", features = ["mcp"] }
```

The `mcp` feature is part of the default feature set.

## Configuration

You configure MCP servers in `.toon` files in your configuration directory. Each server has its own TOON configuration file.

## How it works

1. ARES discovers MCP server configurations in the configuration directory.
2. `McpRegistry::from_dir()` loads the configurations and connects to the servers.
3. Each connected server provides an `McpClient` for tool invocation.
4. Agents access MCP tools through the registry.

## Architecture

```
Agent Request → McpRegistry → get_client("eruka") → McpClient → MCP Server
                                                                      ↓
Agent Response ← Tool Result ←────────────────────────────────────────┘
```

## Library usage

```rust
use ares_mcp::McpRegistry;

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

You configure per-agent MCP access in the TOON configuration of the agent:

```toon
[agent.researcher]
mcp_servers = ["eruka", "search"]
```
