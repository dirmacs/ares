//! ARES agent library facade.
//!
//! Downstream crates inject [`Execute`], [`Tools`], and [`Llm`] on a Cordis
//! [`Context`] and run an agent with no HTTP stack on the graph.
//!
//! ```rust
//! use ares_server::{Context, Execute, Tools, Llm};
//! ```
//!
//! The `ares-server` binary (same package) binds the Axum router; the HTTP
//! stack is an unconditional dependency of this package. Default features are
//! the server defaults: postgres, openai, ares-vector, mcp, inventory,
//! rhai-policy. For an embed-only build use `--no-default-features`.
//!
//! Public surface: [`Context`], [`Execute`], [`Tools`], [`Llm`], [`Store`]
//! (postgres), [`Plugin`], [`Loader`], [`Dispatch`], [`register_plugins`].
//! Construct in-memory [`Llm`] with [`Llm::from_client`] for the library proof.

pub use ares_agent::{AgentConfig, AgentRegistry, AgentRequest, Execute, ExecutionResult};
pub use ares_llm::coordinator::ConversationMessage;
pub use ares_llm::{LLMClient, LLMResponse, Llm};
pub use ares_tools::{Calculator, Tool, Tools};
pub use ares_types::types::ToolDefinition;
pub use ares_types::{AppError, TenantContext, TenantTier};

/// Daemon-side supervised-worker protocol: restart loop and child spawn.
pub mod supervisor;
pub use cordis::{Context, Dispatch, Loader, Plugin, PluginRegistry, Service};

/// Tenant database. Gated so `--no-default-features` builds do not enable
/// postgres.
#[cfg(feature = "postgres")]
pub use ares_store::Store;

/// Register capability-crate loader factories on `reg`.
///
pub fn register_plugins(reg: &PluginRegistry) {
    cordis::register_plugins(reg);
    ares_store::register_plugins(reg);
    ares_tools::register_plugins(reg);
    ares_llm::register_plugins(reg);
    ares_agent::register_plugins(reg);
    ares_http::register_plugins(reg);
}
