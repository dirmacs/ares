// ares/src/mcp/mod.rs
// MCP module — exposes ARES as an MCP tool provider.

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
