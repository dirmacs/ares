#[cfg(feature = "mcp")]
pub mod server;

#[cfg(feature = "mcp")]
pub mod client;

#[cfg(feature = "mcp")]
pub mod registry;

#[cfg(feature = "mcp")]
pub use registry::McpRegistry;
