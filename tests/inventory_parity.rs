//! Parity proof for inventory-collected plugin factories.
//!
//! The server's primary registration path is `cordis::register_inventory_factories`;
//! this test builds a registry through that path alone and asserts the resulting
//! key set matches what the hand-written `register_plugins` chains install under
//! the same feature flags. To guarantee the factory submissions are actually
//! linked in (inventory nodes are dropped by the linker when no code from a
//! crate is retained), each capability crate is referenced via its manual
//! registration first.
//!
//! Feature notes: the root package has no standalone `rhai`/`scheduler`/
//! `pipeline`/`trigger` features — `postgres` (default) pulls the engines and
//! `rhai-policy` (default) pulls Rhai. The `http` feature (default) gates the
//! optional `ares-http` dependency; this whole file is gated on it below, so
//! the `Http` factory is asserted unconditionally here.
#![cfg(feature = "http")]

use cordis::PluginRegistry;

#[test]
fn inventory_registry_matches_expected_factory_set() {
    // Force linkage of every LIBRARY crate contributing factory submissions.
    // (Runtime no-ops: they only fill a throwaway registry.) Server-owned
    // factories (Overlay, HealthJobService, noop_probe) live in this package's
    // src/plugins.rs and are covered by the live boot smoke.
    let force = PluginRegistry::new();
    #[cfg(feature = "postgres")]
    ares_store::register_plugins(&force);
    ares_tools::register_plugins(&force);
    ares_llm::register_plugins(&force);
    ares_agent::register_plugins(&force);
    // ares_http is an optional (`http` feature) dependency and this file
    // only compiles with `http` on; force-link it so its factory
    // submissions survive dead-code elimination.
    ares_http::register_plugins(&force);
    drop(force);

    let reg = PluginRegistry::new();
    cordis::register_inventory_factories(&reg);
    let mut names = reg.names();
    names.sort();

    // Engines (scheduler/pipeline/trigger) ride in behind the default
    // postgres feature; RhaiPolicy rides behind rhai-policy (both default).
    #[cfg(feature = "postgres")]
    let mut expected: Vec<String> = vec![
        "EventsService".to_string(),
        "Store".to_string(),
        "Execute".to_string(),
        "SchedulerService".to_string(),
        "PipelineService".to_string(),
        "TriggerService".to_string(),
        "CalculatorService".to_string(),
        "Tools".to_string(),
        "Llm".to_string(),
        "RhaiPolicy".to_string(),
    ];
    #[cfg(not(feature = "postgres"))]
    let mut expected: Vec<String> = vec![
        "EventsService".to_string(),
        "CalculatorService".to_string(),
        "Tools".to_string(),
        "Llm".to_string(),
        "Execute".to_string(),
    ];
    expected.push("Http".to_string());
    #[cfg(feature = "postgres")]
    expected.push("AuthService".to_string());
    expected.sort();

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
