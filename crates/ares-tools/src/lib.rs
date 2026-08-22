//! Built-in agent tools for A.R.E.S.

pub mod config;
pub use config::ToolConfig;

pub mod tools;
pub mod calculator;
pub mod http_tool;
pub mod registry;
pub mod rhai_tool;
pub mod tool_service;
pub mod script_tool;
pub mod plugins;

#[cfg(any(feature = "postgres", test))]
pub mod runtime_registry;

#[cfg(any(feature = "postgres", test))]
pub mod sql_tool;

#[cfg(any(feature = "search-tools", test))]
pub mod search;

#[cfg(any(feature = "search-tools", test))]
pub mod web_scrape;

#[cfg(any(feature = "mcp", test))]
pub mod mcp_bridge;

#[cfg(any(feature = "postgres", test))]
pub mod connectors;

pub use calculator::{Calculator, CalculatorConfig, CalculatorService};
pub use plugins::register_plugins;
pub use registry::{Tool, ToolRegistry};
pub use rhai_tool::{RhaiTool, RhaiToolConfig};
pub use tool_service::Tools;

#[cfg(any(feature = "postgres", test))]
pub use runtime_registry::RuntimeToolRegistry;

#[cfg(test)]
mod tests {
    use super::{calculator::Calculator, registry::ToolRegistry};
    use std::sync::Arc;

    #[test]
    fn calculator_and_registry_are_wired() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(Calculator));
        assert!(registry.has_tool("calculator"));
        assert_eq!(registry.get("calculator").unwrap().name(), "calculator");
    }

    #[test]
    fn search_and_scrape_modules_compile_under_test_cfg() {
        let _ = std::any::type_name::<crate::search::WebSearch>();
        let _ = std::any::type_name::<crate::web_scrape::WebScrape>();
    }
}
