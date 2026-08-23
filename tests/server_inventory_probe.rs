//! Server-level proof: the ares-server binary links every capability crate,
//! so inventory must collect ALL factory submissions here.
//!
//! The server crate is a binary-only package (no lib target), so its own
//! factory submits (Overlay, HealthJobService, noop_probe in src/plugins.rs)
//! cannot be referenced from an integration test. Instead this test proves
//! collection for every LIBRARY crate the binary links, and the live smoke
//! (boot + entries apply) covers the server-owned factories end to end.

#[test]
fn server_libraries_collect_full_factory_set() {
    // Force linkage of crates whose only other reference would be main.rs's
    // inventory-off fallback path. Inventory nodes are dropped by the linker
    // when no code from a crate is retained.
    let force = cordis::PluginRegistry::new();
    ares::register_plugins(&force);
    ares_http::register_plugins(&force);
    drop(force);

    let reg = cordis::PluginRegistry::new();
    cordis::register_inventory_factories(&reg);
    let mut names = reg.names();
    names.sort();

    for expected in [
        "EventsService",
        "CalculatorService",
        "Tools",
        "Llm",
        "Execute",
        "Http",
        "Store",
        "AuthService",
        "SchedulerService",
        "PipelineService",
        "TriggerService",
        "RhaiPolicy",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "factory `{expected}` missing from inventory collection: {names:?}"
        );
    }
}
