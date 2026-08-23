//! Test database harness for integration tests.
//!
//! Provides proper PostgreSQL test database setup with once-per-binary
//! initialization (cleanup + migrations) and per-test connection pooling.
//!
//! # Environment
//!
//! Tests use `TEST_DATABASE_URL` if set, falling back to `DATABASE_URL` from `.env`
//! rewritten to target `ares_test`. The test database must already exist:
//!
//! ```bash
//! sudo -u postgres psql -c "CREATE DATABASE ares_test OWNER dirmacs;"
//! ```
//!
//! # Usage
//!
//! ```rust
//! let db = create_test_db().await; // connects, ensures schema, cleans once per binary
//! ```

use ares_store::PostgresClient;
use ares_types::types::Result;
use std::sync::Once;

static LOAD_ENV: Once = Once::new();
static INIT_SCHEMA: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// Load `.env` file once per test process, so DATABASE_URL is available.
fn ensure_env_loaded() {
    LOAD_ENV.call_once(|| {
        let _ = dotenvy::dotenv();
    });
}

/// Returns the test database URL.
///
/// Priority:
/// 1. `TEST_DATABASE_URL` env var (CI / custom setups)
/// 2. `DATABASE_URL` env var rewritten to ares_test (loaded from .env)
/// 3. Fallback: postgres://dirmacs@localhost/ares_test (uses pg_hba trust or .pgpass)
pub fn test_db_url() -> String {
    ensure_env_loaded();

    if let Ok(url) = std::env::var("TEST_DATABASE_URL") {
        return url;
    }
    if let Ok(url) = std::env::var("DATABASE_URL") {
        // Rewrite to use ares_test instead of ares production DB
        if url.contains("/ares") && !url.contains("ares_test") {
            return url.replace("/ares", "/ares_test");
        }
        return url;
    }
    "postgres://dirmacs@localhost:5432/ares_test".to_string()
}

/// Connect to the test DB. On first call per binary, runs cleanup + migrations.
/// Subsequent calls just connect (schema is already ready).
pub async fn create_test_db() -> PostgresClient {
    let url = test_db_url();
    let db = PostgresClient::new_remote(url, String::new())
        .await
        .expect("Failed to connect to ares_test. Ensure it exists and migrations are applied.");

    // One-time init: truncate stale data from prior runs, then ensure schema
    if INIT_SCHEMA.set(()).is_ok() {
        cleanup_tables(&db).await;
        ensure_schema(&db).await.expect("Failed to run migrations on ares_test");
    }

    db
}

/// Truncate all application tables. Called once at start of test binary.
async fn cleanup_tables(db: &PostgresClient) {
    let tables = [
        "messages",
        "conversations",
        "sessions",
        "memory_facts",
        "preferences",
        "user_agents",
        "users",
    ];

    for table in &tables {
        let query = format!("TRUNCATE TABLE {} CASCADE", table);
        if let Err(e) = sqlx::query(&query).execute(&db.pool).await {
            eprintln!("Warning: failed to truncate {}: {}", table, e);
        }
    }
}

/// Run schema migrations against the test database pool.
pub async fn ensure_schema(db: &PostgresClient) -> Result<()> {
    sqlx::migrate!("./migrations")
        .run(&db.pool)
        .await
        .map_err(|e| ares_types::types::AppError::Database(format!("Migration failed: {}", e)))?;
    Ok(())
}
