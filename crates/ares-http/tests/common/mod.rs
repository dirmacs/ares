//! Shared gate for the live-database audit tests (item 1.16).
//!
//! A live test must never turn "the database is not there" into a green
//! result when the run was configured to have one, and must never print the
//! database URL (it can carry a password):
//!
//! - `TEST_DATABASE_URL` **set** and the database unreachable: panic, with a
//!   message that names the variable and never its value. A configured run
//!   does not skip.
//! - `TEST_DATABASE_URL` **unset**: the crate's existing skip convention
//!   (`live_chat_stream.rs`): resolve the URL the way `ares-test-support`
//!   does, and if nothing is reachable print `SKIPPED ..` and return.

#![allow(dead_code)]

use std::time::Duration;

/// The variable that configures a live run.
pub const DB_ENV: &str = "TEST_DATABASE_URL";

/// Cheap reachability probe: can a connection be opened within 5 seconds.
pub async fn reachable(url: &str) -> bool {
    match tokio::time::timeout(
        Duration::from_secs(5),
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(url),
    )
    .await
    {
        Ok(Ok(pool)) => {
            pool.close().await;
            true
        }
        _ => false,
    }
}

/// What the gate decided.
pub enum Gate {
    /// The database is reachable: run the test against this URL.
    Run(String),
    /// Unconfigured and nothing reachable: skip, for this reason (no URL in it).
    Skip(String),
}

/// The gate itself, with the two inputs made explicit so it can be tested
/// without touching the process environment. Prints nothing.
///
/// [`Gate::Run`] when the database is reachable. Unreachable and `configured`:
/// panics, naming [`DB_ENV`] only. Unreachable and not configured:
/// [`Gate::Skip`].
pub async fn gate(test: &str, configured: bool, url: String) -> Gate {
    if reachable(&url).await {
        return Gate::Run(url);
    }
    if configured {
        panic!(
            "{test}: {DB_ENV} is set but the database it names is unreachable (no connection \
             within 5 s). A configured run never skips: fix the variable or the database."
        );
    }
    Gate::Skip(format!(
        "{test}: no test database reachable ({DB_ENV} is unset; checked DATABASE_URL and the \
         unix-socket fallback)"
    ))
}

/// [`gate`], with the unconfigured skip printed as `SKIPPED ..` (no URL).
pub async fn resolve(test: &str, configured: bool, url: String) -> Option<String> {
    match gate(test, configured, url).await {
        Gate::Run(url) => Some(url),
        Gate::Skip(reason) => {
            eprintln!("SKIPPED {reason}");
            None
        }
    }
}

/// The URL of the live test database, or `None` for the unconfigured skip.
pub async fn live_db_url(test: &str) -> Option<String> {
    let configured = std::env::var(DB_ENV).is_ok();
    resolve(test, configured, ares_test_support::test_db_url()).await
}

/// The running test's name (libtest names the test thread after the test).
pub fn current_test_name() -> String {
    std::thread::current()
        .name()
        .unwrap_or("live test")
        .to_string()
}
