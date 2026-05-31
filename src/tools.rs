//! Built-in agent tools for A.R.E.S.
pub use ares_tools::*;

#[cfg(test)]
mod tests {
    use super::{calculator::Calculator, ToolRegistry};
    use std::sync::Arc;

    #[test]
    fn tools_crate_reexport_registers_calculator() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(Calculator));
        assert!(registry.has_tool("calculator"));
    }
}
