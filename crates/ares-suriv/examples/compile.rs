//! Compile a Suriv harness and print what it compiles to.
//!
//! ```console
//! $ cargo run -p ares-suriv --example compile -- crates/ares-suriv/tests/fixtures/billing-flap.suriv
//! ```

use ares_suriv::codegen::compile_source;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: compile <harness.suriv>")?;
    let source = std::fs::read_to_string(&path)?;
    let compiled = compile_source(&source).map_err(|e| format!("{path}: {e}"))?;
    for entry in &compiled.entries {
        println!(
            "ENTRY id={} plugin={} isolate={:?}",
            entry.id, entry.plugin, entry.isolate
        );
        println!("   system_prompt={:?}", entry.config.system_prompt);
    }
    for (id, services) in &compiled.requires {
        println!("REQUIRES {id} -> {services:?}");
    }
    Ok(())
}
