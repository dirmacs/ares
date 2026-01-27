//! Built-in Tools for Agent Capabilities
//!
//! This module provides the tool infrastructure that enables agents to perform
//! actions beyond text generation, such as calculations and web searches.
//!
//! # Module Structure
//!
//! - [`calculator`](crate::tools::calculator) - Mathematical expression evaluation
//! - [`search`](crate::tools::search) - Web search integration (DuckDuckGo, Brave, etc.)
//! - [`registry`](crate::tools::registry) - Tool registration and discovery
//!
//! # Available Tools
//!
//! ## Calculator
//! Evaluates mathematical expressions safely:
//! ```ignore
//! let result = calculator::evaluate("2 + 2 * 3")?;  // Returns 8
//! ```
//!
//! ## Web Search
//! Searches the web and returns relevant results:
//! ```ignore
//! let results = search::web_search("rust programming", 5).await?;
//! for result in results {
//!     println!("{}: {}", result.title, result.url);
//! }
//! ```
//!
//! # Tool Registry
//!
//! The [`registry`](crate::tools::registry) module manages tool discovery and execution:
//! ```ignore
//! let registry = ToolRegistry::default();
//! let tools = registry.list_tools();  // Get available tool schemas
//! let result = registry.execute("calculator", json!({"expr": "2+2"})).await?;
//! ```
//!
//! # MCP Integration
//!
//! Tools can also be provided via MCP (Model Context Protocol) servers.
//! See the `mcp` module for external tool integration.

/// Calculator tool for arithmetic operations.
pub mod calculator;
/// Tool registry for managing available tools.
pub mod registry;
/// Web search tool using DuckDuckGo.
pub mod search;
