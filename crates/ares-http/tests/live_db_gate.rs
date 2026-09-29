//! The live-database gate (`tests/common/mod.rs`) used by the audit live
//! tests: a configured run must fail loudly when the database is missing,
//! and must never echo the database URL.
//!
//! Pure: it points the gate at a closed local port, opens no real database
//! and does not read or write the process environment.

#![cfg(feature = "postgres")]

mod common;

/// Nothing listens on port 1, so the probe fails fast. The user and database
/// names are sentinels: if either shows up in a panic message, the URL leaked.
const UNREACHABLE: &str = "postgres://sentinel-user@127.0.0.1:1/sentinel-db";

fn run<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime")
        .block_on(f)
}

#[test]
fn configured_run_with_an_unreachable_database_panics_naming_the_variable_only() {
    let outcome = std::panic::catch_unwind(|| {
        run(common::resolve("gate_probe", true, UNREACHABLE.to_string()))
    });
    let payload = outcome.expect_err("a configured run with no database must panic, not skip");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .expect("string panic payload");

    assert!(
        message.contains(common::DB_ENV),
        "the message must name the variable: {message}"
    );
    for leak in ["sentinel-user", "sentinel-db", "127.0.0.1", "postgres://"] {
        assert!(
            !message.contains(leak),
            "the message must not echo any part of the URL ({leak:?}): {message}"
        );
    }
}

#[test]
fn unconfigured_run_with_an_unreachable_database_skips_without_panicking() {
    // `gate`, not `resolve`: the skip is returned, not printed, so no SKIPPED
    // line appears in this binary's output.
    let got = run(common::gate("gate_probe", false, UNREACHABLE.to_string()));
    let common::Gate::Skip(reason) = got else {
        panic!("an unconfigured run with no database must skip");
    };
    assert!(reason.contains(common::DB_ENV), "{reason}");
    for leak in ["sentinel-user", "sentinel-db", "127.0.0.1", "postgres://"] {
        assert!(
            !reason.contains(leak),
            "the skip reason must not echo any part of the URL ({leak:?}): {reason}"
        );
    }
}
