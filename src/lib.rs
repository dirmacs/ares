//! ARES agent library facade.
//!
//! Downstream crates inject [`Execute`], [`Tools`], and [`Llm`] on a Cordis
//! [`Context`] and run an agent with no HTTP stack on the graph.
//!
//! ```rust
//! use ares_server::{Context, Execute, Tools, Llm};
//! ```
//!
//! The `ares-server` binary (same package) binds the Axum router only when
//! the `http` feature is on, and ships only with `http` and `cli`. Default
//! features are the server defaults: postgres, openai, ares-vector, mcp,
//! inventory, rhai-policy, http, cli, script-tools. For an embed-only build
//! use `default-features = false` and pick the providers you need (e.g.
//! `features = ["openai"]`); no axum, clap, sqlx, ollama, boa, or rhai then
//! enters the graph.
//!
//! Public surface: [`Context`], [`Execute`], [`Tools`], [`Llm`], [`Store`]
//! (postgres), [`Plugin`], [`Loader`], [`Dispatch`], [`register_plugins`].
//! Construct in-memory [`Llm`] with [`Llm::from_client`] for the library proof.

pub use ares_agent::{AgentConfig, AgentRegistry, AgentRequest, Execute, ExecutionResult};
pub use ares_llm::coordinator::ConversationMessage;
pub use ares_llm::{LLMClient, LLMResponse, Llm};
pub use ares_tools::{Calculator, Tool, Tools};
pub use ares_types::types::ToolDefinition;
pub use ares_types::types::{Message, ToolCall};
pub use ares_types::{AppError, TenantContext, TenantTier};

pub use ares_agent::{AgentResponse, AgentSource, ContextProvider, ExecutionMetadata, RunTracker};
pub use ares_llm::client::TokenUsage;
pub use ares_llm::{ProviderConfig, ProviderRegistry};
pub use ares_tools::{CalculatorConfig, CalculatorService};

pub use cordis::loader::Entry;
pub use cordis::{CordisError, Disposable, EventsService, FiberId, ServiceInitFuture, TypedEvent};

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
    #[cfg(feature = "http")]
    ares_http::register_plugins(reg);
}
