//! Built-in agent tools for A.R.E.S.
pub use ares_tools::*;

#[cfg(test)]
mod tests {
    use super::{calculator::Calculator, Tool, Tools};
    use cordis::Context;
    use std::sync::Arc;

    #[test]
    fn tools_crate_reexport_registers_calculator() {
        let tools = Tools::from_static([Arc::new(Calculator) as Arc<dyn Tool>]);
        let ctx = Context::new_root();
        let names: Vec<String> = tools.list(&ctx).into_iter().map(|d| d.name).collect();
        assert!(names.iter().any(|n| n == "calculator"));
        assert!(tools.resolve(&ctx, "calculator").is_some());
    }
}
