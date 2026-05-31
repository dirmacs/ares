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

#[cfg(test)]
mod tests {
    use super::{calculator::Calculator, registry::ToolRegistry, Tool};
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
