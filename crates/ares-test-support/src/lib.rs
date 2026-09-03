//! Shared live-test database harness for ARES integration tests.
//!
//! One resolver, one lifecycle, one failure policy for every crate's live
//! Postgres tests (`ares_test`):
//!
//! - URL resolution: `TEST_DATABASE_URL` -> `DATABASE_URL` (postgres URLs only,
//!   rewritten to `ares_test`) -> unix-socket peer auth, no credentials needed.
//! - Lifecycle: once per test binary — connect, run migrations, truncate; the
//!   tables are truncated again when the binary exits, so `ares_test` is left
//!   empty between runs.
//! - Failure policy: a configured-but-unreachable database fails loudly with
//!   fix instructions instead of panicking mid-test or silently skipping.

use std::future::Future;
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

/// Load `.env` once per test process, so `DATABASE_URL` is available if present.
fn ensure_env_loaded() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = dotenvy::dotenv();
    });
}

/// Returns the test database URL.
///
/// Priority:
/// 1. `TEST_DATABASE_URL` env var (CI / custom setups)
/// 2. `DATABASE_URL` env var rewritten to `ares_test` (postgres URLs only;
///    non-postgres values such as the sqlite path in `.env.example` are ignored)
/// 3. Fallback: unix-socket peer auth (`/var/run/postgresql`), which works with
///    zero configuration for the OS user that owns `ares_test`
pub fn test_db_url() -> String {
    ensure_env_loaded();

    if let Ok(url) = std::env::var("TEST_DATABASE_URL") {
        return url;
    }
    if let Ok(url) = std::env::var("DATABASE_URL") {
        if url.starts_with("postgres") {
            if url.contains("/ares") && !url.contains("ares_test") {
                return url.replace("/ares", "/ares_test");
            }
            return url;
        }
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "postgres".into());
    format!("postgres://{user}@%2Fvar%2Frun%2Fpostgresql/ares_test")
}

/// Panic with actionable fix instructions for an unreachable test database.
fn unreachable_panic(url: &str, reason: impl std::fmt::Display) -> ! {
    panic!(
        "live test DB unreachable at {url}\n\
         Fix one of:\n  \
         1. start postgres: sudo systemctl start postgresql\n  \
         2. create the test DB: sudo -u postgres psql -c \"CREATE DATABASE ares_test OWNER $USER;\"\n  \
         3. export TEST_DATABASE_URL=postgres://user:pass@host/ares_test\n\
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

fn connect_or_panic(url: &str) -> impl Future<Output = sqlx::PgPool> {
    let url = url.to_string();
    async move {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .unwrap_or_else(|e| unreachable_panic(&url, e))
    }
}

async fn init() {
    let url = test_db_url();
    let pool = connect_or_panic(&url).await;
    if let Err(e) = ares_store::MIGRATOR.run(&pool).await {
        unreachable_panic(&url, format!("migration failed: {e}"));
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
#[dtor::dtor]
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

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn fallback_is_socket_peer_auth() {
        let _g = env_lock();
        let saved_test = std::env::var("TEST_DATABASE_URL").ok();
        let saved_db = std::env::var("DATABASE_URL").ok();
        std::env::remove_var("TEST_DATABASE_URL");
        std::env::remove_var("DATABASE_URL");
        let url = test_db_url();
        if let Some(v) = saved_test {
            std::env::set_var("TEST_DATABASE_URL", v);
        }
        if let Some(v) = saved_db {
            std::env::set_var("DATABASE_URL", v);
        }
        assert!(
            url.contains("%2Fvar%2Frun%2Fpostgresql"),
            "unexpected: {url}"
        );
        assert!(url.ends_with("/ares_test"), "unexpected: {url}");
        assert!(
            !url.contains("localhost"),
            "no TCP fallback expected: {url}"
        );
    }

    #[test]
    fn sqlite_database_url_is_ignored() {
        let _g = env_lock();
        let saved_test = std::env::var("TEST_DATABASE_URL").ok();
        let saved_db = std::env::var("DATABASE_URL").ok();
        std::env::remove_var("TEST_DATABASE_URL");
        std::env::set_var("DATABASE_URL", "./data/ares.db");
        let url = test_db_url();
        if let Some(v) = saved_test {
            std::env::set_var("TEST_DATABASE_URL", v);
        }
        if let Some(v) = saved_db {
            std::env::set_var("DATABASE_URL", v);
        }
        assert!(
            url.contains("%2Fvar%2Frun%2Fpostgresql"),
            "unexpected: {url}"
        );
    }

    /// Local smoke test: run with `cargo test -p ares-test-support -- --ignored`.
    #[tokio::test]
    #[ignore = "requires a live ares_test database"]
    async fn connects_via_socket_without_credentials() {
        std::env::remove_var("TEST_DATABASE_URL");
        std::env::remove_var("DATABASE_URL");
        let pool = pool().await;
        let row: (i32,) = sqlx::query_as("SELECT 1").fetch_one(&pool).await.unwrap();
        assert_eq!(row.0, 1);
    }
}
