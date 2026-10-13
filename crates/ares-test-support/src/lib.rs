//! Shared live-test database harness for ARES integration tests.
//!
//! One resolver, one lifecycle, one failure policy for every crate's live
//! Postgres tests (`ares_test`):
//!
//! - URL resolution: an explicit `TEST_DATABASE_URL` naming the local
//!   `ares_test` database is required.
//! - Lifecycle: once per test binary — connect, run migrations, truncate; the
//!   tables are truncated again when the binary exits, so `ares_test` is left
//!   empty between runs.
//! - Failure policy: a configured-but-unreachable database fails loudly with
//!   fix instructions instead of panicking mid-test or silently skipping.

use std::future::Future;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};

static INIT: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
static READY: AtomicBool = AtomicBool::new(false);

/// Application tables truncated before and after each test binary run.
const CLEANUP_TABLES: &[&str] = &[
    "messages",
    "conversations",
    "sessions",
    "memory_facts",
    "preferences",
    "user_agents",
    "users",
];

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
pub fn test_db_url() -> String {
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

/// Truncate all application tables.
async fn truncate_all(pool: &sqlx::PgPool) {
    for table in CLEANUP_TABLES {
        let query = format!("TRUNCATE TABLE {table} CASCADE");
        if let Err(e) = sqlx::query(&query).execute(pool).await {
            eprintln!("Warning: failed to truncate {table}: {e}");
        }
    }
}

fn connect_or_panic(url: &str) -> impl Future<Output = sqlx::PgPool> + use<> {
    let url = url.to_string();
    async move {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .unwrap_or_else(|e| unreachable_panic(e))
    }
}

async fn init() {
    let url = test_db_url();
    let pool = connect_or_panic(&url).await;
    if let Err(e) = ares_store::MIGRATOR.run(&pool).await {
        unreachable_panic(format!("migration failed: {e}"));
    }
    truncate_all(&pool).await;
    READY.store(true, Ordering::SeqCst);
}

/// Connect to `ares_test`.
///
/// First call per test binary: connect, run migrations, truncate stale data.
/// Every call returns a pool owned by the caller's runtime — test binaries run
/// many short-lived tokio runtimes, and a pool shared across them outlives the
/// runtime it was created on. An unreachable database panics with fix
/// instructions — live coverage is never silently skipped.
pub async fn pool() -> sqlx::PgPool {
    INIT.get_or_init(init).await;
    connect_or_panic(&test_db_url()).await
}

/// A fresh connection wrapped as [`ares_store::PostgresClient`].
pub async fn client() -> ares_store::PostgresClient {
    ares_store::PostgresClient { pool: pool().await }
}

/// Truncate again when the test binary exits, leaving `ares_test` empty.
/// Runs only if this binary actually connected; teardown errors are reported
/// but never mask test results.
// On macOS the `dtor` hook runs after Rust has destroyed the main thread's
// thread-local state. Building a Tokio runtime there panics before it can
// clean the disposable test database. CI and Linux retain the exit cleanup;
// on macOS the next invocation still clears the database during `init`.
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
    // A fresh connection is required here: connections held by `POOL` belong to
    // the (now finished) test runtime and cannot be reused after main returns.
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
                "postgres://tester@localhost/ares_test?dbname=live".to_string()
            )),
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
