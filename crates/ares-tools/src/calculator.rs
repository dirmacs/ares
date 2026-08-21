// Shim re-export for backward compatibility.
// Canonical implementation lives in crate::tools::calculator.
// This file stays for one commit so existing imports keep working.
pub use crate::tools::calculator::{Calculator, CalculatorConfig, CalculatorService};
