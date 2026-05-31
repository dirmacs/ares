// ares/src/mcp/mod.rs
// MCP module — exposes ARES as an MCP tool provider.
//
// Feature layout:
//   `mcp`                — protocol glue, client, auth, extension, registry, tools.
//                          Zero postgres dependency. Enables MCP *consumers*.
//   `mcp` + `postgres`   — adds the MCP *server* (server.rs) and usage tracking
//                          (usage.rs), which query tenant DB via sqlx::PgPool.
//
// This split exists so downstream library consumers can enable `mcp` for client
// and registry code without being forced to pull sqlx + postgres into their
// dependency graph.

#[cfg(feature = "mcp")]
pub mod extension;

#[cfg(all(feature = "mcp", feature = "postgres"))]
pub mod server;

#[cfg(feature = "mcp")]
pub mod tools;

// auth.rs uses ares_db::tenants::TenantDb (postgres-only), so the whole
// module is gated alongside the server. Its only consumer is mcp::server.
#[cfg(feature = "mcp")]
pub mod auth;

#[cfg(all(feature = "mcp", feature = "postgres"))]
pub mod usage;

#[cfg(feature = "mcp")]
pub mod client;

#[cfg(feature = "mcp")]
pub mod registry;

#[cfg(all(feature = "mcp", feature = "postgres"))]
pub use server::start_mcp_server;

#[cfg(feature = "mcp")]
pub use extension::{McpToolExtension, NoOpMcpExtension};

#[cfg(feature = "mcp")]
pub use registry::McpRegistry;
