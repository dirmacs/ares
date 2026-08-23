//! Parity proof for inventory-collected plugin factories.
//!
//! The server's primary registration path is `cordis::register_inventory_factories`;
//! this test builds a registry through that path alone and asserts the resulting
//! key set matches what the hand-written `register_plugins` chains install under
//! the same feature flags. To guarantee the factory submissions are actually
//! linked in (inventory nodes are dropped by the linker when no code from a
//! crate is retained), each capability crate is referenced via its manual
//! registration first.

use cordis::PluginRegistry;

#[test]
fn inventory_registry_matches_expected_factory_set() {
    // Force linkage of every LIBRARY crate contributing factory submissions.
    // (Runtime no-ops: they only fill a throwaway registry.) Server-owned
    // factories (Overlay, HealthJobService, noop_probe) live in the binary
    // crate and are covered by the server-level probe + live smoke instead.
    let force = PluginRegistry::new();
    #[cfg(feature = "postgres")]
    ares_store::register_plugins(&force);
    ares_tools::register_plugins(&force);
    ares_llm::register_plugins(&force);
    ares_agent::register_plugins(&force);
    #[cfg(feature = "http")]
    ares_http::register_plugins(&force);
    drop(force);

    let reg = PluginRegistry::new();
    cordis::register_inventory_factories(&reg);
    let mut names = reg.names();
    names.sort();

    let mut expected: Vec<String> = vec![
        "EventsService".to_string(),
        "CalculatorService".to_string(),
        "Tools".to_string(),
        "Llm".to_string(),
        "Execute".to_string(),
        "Http".to_string(),
        #[cfg(feature = "postgres")]
        "Store".to_string(),
        #[cfg(feature = "postgres")]
        "AuthService".to_string(),
        #[cfg(feature = "rhai")]
        "RhaiPolicy".to_string(),
        #[cfg(feature = "scheduler")]
        "SchedulerService".to_string(),
        #[cfg(feature = "pipeline")]
        "PipelineService".to_string(),
        #[cfg(feature = "trigger")]
        "TriggerService".to_string(),
    ];
    expected.sort();
    expected.dedup();

    assert_eq!(
        names, expected,
        "inventory-collected factories diverge from the manual registration set"
    );
}

#[test]
fn inventory_registry_is_nonempty() {
    let force = PluginRegistry::new();
    ares_tools::register_plugins(&force);
    drop(force);

    let reg = PluginRegistry::new();
    cordis::register_inventory_factories(&reg);
    assert!(
        !reg.names().is_empty(),
        "inventory collected no factories; linker sections may be dropped"
    );
}
