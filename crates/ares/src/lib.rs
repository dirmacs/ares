//! ARES agent library.
//!
//! Downstream crates inject [`Execute`], [`Tools`], and [`Llm`] on a Cordis
//! [`Context`] and run an agent with no HTTP stack on the graph.
//!
//! ```rust
//! use ares::{Context, Execute, Tools, Llm};
//! ```
//!
//! Enable `http` to pull the optional Axum adapter. Default features are empty:
//! no `axum`, no postgres, no engines.
//!
//! Public surface: [`Context`], [`Execute`], [`Tools`], [`Llm`], [`Store`]
//! (postgres), [`Plugin`], [`Loader`], [`Dispatch`], [`register_plugins`].
//! Construction helpers used by `tests/no_http.rs` stay exported so the
//! library proof can provide in-memory [`Llm`] / [`Tools`] / [`Execute`].

pub use ares_agent::{AgentConfig, AgentRegistry, AgentRequest, Execute, ExecutionResult};
pub use ares_llm::coordinator::ConversationMessage;
pub use ares_llm::{ClientPool, Llm, LLMClient, LLMResponse};
pub use ares_tools::{Calculator, Tool, Tools};
pub use ares_types::types::ToolDefinition;
pub use ares_types::{AppError, TenantContext, TenantTier};
pub use cordis::{Context, Dispatch, Loader, Plugin, PluginRegistry, Service};

/// Tenant database. Gated so default features do not enable postgres.
#[cfg(feature = "postgres")]
pub use ares_store::Store;

/// Register capability-crate loader factories on `reg`.
///
/// `ares_http` is included only when the `http` feature is enabled.
pub fn register_plugins(reg: &PluginRegistry) {
    cordis::register_plugins(reg);
    ares_store::register_plugins(reg);
    ares_tools::register_plugins(reg);
    ares_llm::register_plugins(reg);
    ares_agent::register_plugins(reg);
    #[cfg(feature = "http")]
    ares_http::register_plugins(reg);
}
