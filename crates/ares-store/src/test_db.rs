//! Test-only helpers for the live-Postgres tests.
//!
//! Every `#[cfg(test)]` integration test in this crate talks to the same
//! `ares_test` database, and libtest runs them in parallel. Several tests
//! clean up rows by name prefix, so a sibling test can lose the fixtures it is
//! asserting on. [`pool`] serialises the DB-using bodies: it takes a
//! session-level Postgres advisory lock for the caller's whole test. The lock
//! works across threads and across test binaries. Pure unit tests stay fully
//! parallel.
//!
//! This module deliberately does **not** go through `ares-test-support`. That
//! crate depends back on `ares-store` (it runs our `MIGRATOR` and builds our
//! `PostgresClient`), so a dev-dependency on it made the two crates cyclic.
//! `cargo package` resolves the whole optional-dependency space regardless of
//! features, and through that cycle it pulled `sqlx-sqlite` into `ares-store`'s
//! packaged graph alongside `rusqlite`; both claim `links = "sqlite3"`, so
//! `ares-store` could not be packaged at all. The lifecycle here is small
//! enough to own directly, which keeps the dependency one-way.

use std::net::IpAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};

use sqlx::{Connection, PgConnection, PgPool};

/// Fixed advisory-lock key shared by every live-DB test in this crate.
///
/// Arbitrary value; hex spells `ares_sto`.
const TEST_LOCK_KEY: i64 = 0x6172_6573_5F73_746F;

/// Application tables truncated before each test binary run.
///
/// Derived from the live schema rather than listed here: `ares-store` owns
/// `MIGRATOR`, so a hardcoded list is a mirror that silently rots the first
/// time a migration adds a table. The database is the authority.
async fn cleanup_tables(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT tablename FROM pg_tables \
         WHERE schemaname = 'public' AND tablename <> '_sqlx_migrations'",
    )
    .fetch_all(pool)
    .await
}

/// Set once this binary has migrated and truncated.
static INIT: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
static READY: AtomicBool = AtomicBool::new(false);

/// Guard that holds the crate-wide test lock until it is dropped.
pub struct TestLock {
    /// Dedicated session that owns the session-level advisory lock. The lock
    /// releases when this session closes on drop.
    _session: PgConnection,
}

/// Load `.env` once per test process so an explicit test URL can be supplied.
fn ensure_env_loaded() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = dotenvy::dotenv();
    });
}

fn validate_test_db_url(url: &str) -> Result<(), &'static str> {
    let options =
        sqlx::postgres::PgConnectOptions::from_str(url).map_err(|_| "invalid PostgreSQL URL")?;
    let host = options.get_host();
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !loopback {
        return Err("host must be loopback");
    }
    if options.get_database() != Some("ares_test") {
        return Err("database must be named ares_test");
    }
    Ok(())
}

fn checked_test_db_url(url: Option<String>) -> Result<String, &'static str> {
    let url = url.ok_or("TEST_DATABASE_URL is required")?;
    validate_test_db_url(&url)?;
    Ok(url)
}

/// Returns the explicitly configured, validated ARES test database URL.
///
/// Panics rather than falling back to a default: a live test that silently
/// picked a different database would be worse than one that refuses to run.
pub(crate) fn test_db_url() -> String {
    ensure_env_loaded();
    checked_test_db_url(std::env::var("TEST_DATABASE_URL").ok())
        .unwrap_or_else(|reason| panic!("Unsafe TEST_DATABASE_URL for ARES tests: {reason}"))
}

/// Panic with actionable fix instructions for an unreachable test database.
fn unreachable_panic(reason: impl std::fmt::Display) -> ! {
    panic!(
        "live ARES test database unreachable\n\
         Fix one of:\n  \
         1. start postgres: sudo systemctl start postgresql\n  \
         2. create the test DB: sudo -u postgres psql -c \"CREATE DATABASE ares_test OWNER $USER;\"\n  \
         3. export TEST_DATABASE_URL=postgres://user@127.0.0.1:5432/ares_test\n\
         underlying error: {reason}"
    )
}

fn connect_or_panic(url: &str) -> impl std::future::Future<Output = PgPool> + use<> {
    let url = url.to_string();
    async move {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .unwrap_or_else(|e| unreachable_panic(e))
    }
}

/// Truncate every application table.
async fn truncate_all(pool: &PgPool) {
    let tables = match cleanup_tables(pool).await {
        Ok(tables) => tables,
        Err(e) => {
            eprintln!("Warning: could not list tables to truncate: {e}");
            return;
        }
    };
    if tables.is_empty() {
        return;
    }
    // One statement, so a partial failure cannot leave the database half-cleared.
    let query = format!(
        "TRUNCATE TABLE {} CASCADE",
        tables
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Err(e) = sqlx::query(&query).execute(pool).await {
        eprintln!("Warning: failed to truncate test database: {e}");
    }
}

/// Once per test binary: migrate, then clear anything a previous run left.
async fn init() {
    let pool = connect_or_panic(&test_db_url()).await;
    if let Err(e) = crate::MIGRATOR.run(&pool).await {
        unreachable_panic(format!("migration failed: {e}"));
    }
    truncate_all(&pool).await;
    READY.store(true, Ordering::SeqCst);
    pool.close().await;
}

/// Connect the shared live-test pool, then take the crate-wide test lock.
///
/// First call per test binary migrates and truncates. Every call returns a pool
/// owned by the caller's runtime — test binaries run many short-lived tokio
/// runtimes, and a pool shared across them outlives the runtime it was created
/// on. An unreachable database panics with fix instructions, so live coverage is
/// never silently skipped.
///
/// Keep the returned guard alive for the whole test body.
pub async fn pool() -> (TestLock, PgPool) {
    INIT.get_or_init(init).await;
    let url = test_db_url();
    let pool = connect_or_panic(&url).await;
    let mut session = PgConnection::connect(&url)
        .await
        .expect("connect test DB lock session");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(TEST_LOCK_KEY)
        .execute(&mut session)
        .await
        .expect("take test DB advisory lock");
    (TestLock { _session: session }, pool)
}

/// Truncate again when the test binary exits, leaving `ares_test` empty.
///
/// Runs only if this binary actually connected; teardown errors are reported but
/// never mask test results.
// On macOS the `dtor` hook runs after Rust has destroyed the main thread's
// thread-local state. Building a Tokio runtime there panics before it can clean
// the disposable test database. CI and Linux retain the exit cleanup; on macOS
// the next invocation still clears the database during `init`.
#[cfg_attr(not(target_os = "macos"), dtor::dtor)]
#[cfg(not(target_os = "macos"))]
unsafe fn truncate_after_run() {
    if !READY.load(Ordering::SeqCst) {
        return;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Warning: test-support teardown could not build a runtime: {e}");
            return;
        }
    };
    // A fresh connection is required here: the connections handed out by `pool`
    // belong to the (now finished) test runtime and cannot be reused after main
    // returns.
    runtime.block_on(async {
        let pool = match sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect(&test_db_url())
            .await
        {
            Ok(pool) => pool,
            Err(e) => {
                eprintln!("Warning: test-support teardown could not connect: {e}");
                return;
            }
        };
        truncate_all(&pool).await;
        pool.close().await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_only_loopback_ares_test_database() {
        assert!(
            checked_test_db_url(Some(
                "postgres://tester@127.0.0.1:5432/ares_test".to_string()
            ))
            .is_ok()
        );
        assert!(
            checked_test_db_url(Some(
                "postgres://tester@localhost:5432/ares_test".to_string()
            ))
            .is_ok()
        );
        assert_eq!(
            checked_test_db_url(Some("postgres://tester@example.com/ares_test".to_string())),
            Err("host must be loopback")
        );
        assert_eq!(
            checked_test_db_url(Some("postgres://tester@localhost/ares".to_string())),
            Err("database must be named ares_test")
        );
        assert_eq!(
            checked_test_db_url(Some(
                "postgres://tester@localhost/ares_test?host=remote.example".to_string()
            )),
            Err("host must be loopback")
        );
    }

    #[test]
    fn refuses_missing_or_non_postgres_test_url() {
        assert_eq!(
            checked_test_db_url(None),
            Err("TEST_DATABASE_URL is required")
        );
        assert_eq!(
            checked_test_db_url(Some("./data/ares.db".to_string())),
            Err("invalid PostgreSQL URL")
        );
    }
}
