// ares/src/mcp/mod.rs
// MCP module — exposes ARES as an MCP tool provider.
//
// Submodules:
//   server       — MCP server with tool definitions and handlers
//   tools        — Input/output type definitions for all tools
//   auth         — API key extraction and validation for MCP sessions
//   usage        — Usage tracking for billing
//   eruka_proxy  — Proxy layer for Eruka read/write/search
//   client       — MCP client for calling external MCP servers (from ares-eruka-wiring)
//   registry     — MCP client registry

#[cfg(feature = "mcp")]
pub mod server;

#[cfg(feature = "mcp")]
pub mod tools;

#[cfg(feature = "mcp")]
pub mod auth;

#[cfg(feature = "mcp")]
pub mod usage;

#[cfg(feature = "mcp")]
pub mod eruka_proxy;

#[cfg(feature = "mcp")]
pub mod client;

#[cfg(feature = "mcp")]
pub mod registry;

#[cfg(feature = "mcp")]
pub use server::start_mcp_server;

#[cfg(feature = "mcp")]
pub use registry::McpRegistry;
