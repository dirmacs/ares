//! Built-in agent tools for A.R.E.S.

pub mod calculator;
pub mod registry;

#[cfg(any(feature = "search-tools", test))]
pub mod search;

#[cfg(any(feature = "search-tools", test))]
pub mod web_scrape;

#[cfg(feature = "mcp")]
pub mod mcp_bridge;

pub use registry::{Tool, ToolRegistry};
