//! Built-in agent tools for A.R.E.S.

pub mod config;
pub use config::ToolConfig;

pub mod calculator;
pub mod fence;
pub mod http_tool;
pub mod plugins;
pub(crate) mod registry;
pub mod rhai_tool;
pub mod script_tool;
pub mod tool_service;
pub mod tools;

#[cfg(any(feature = "postgres", test))]
pub(crate) mod runtime_registry;

#[cfg(any(feature = "postgres", test))]
pub(crate) use runtime_registry::RuntimeToolRegistry;

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
pub use registry::Tool;
pub use rhai_tool::{RhaiTool, RhaiToolConfig};
pub use tool_service::Tools;

#[cfg(test)]
mod tests {
    use super::{calculator::Calculator, Tool, Tools};
    use std::sync::Arc;

    #[test]
    fn calculator_and_tools_are_wired() {
        let tools = Tools::from_static([Arc::new(Calculator) as Arc<dyn Tool>]);
        let ctx = cordis::Context::new_root();
        assert!(tools.resolve(&ctx, "calculator").is_some());
        assert!(tools
            .list(&ctx)
            .iter()
            .any(|definition| definition.name == "calculator"));
    }

    #[test]
    fn search_and_scrape_modules_compile_under_test_cfg() {
        let _ = std::any::type_name::<crate::search::WebSearch>();
        let _ = std::any::type_name::<crate::web_scrape::WebScrape>();
    }
}
