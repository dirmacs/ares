//! Test database harness for integration tests.
//!
//! Thin delegation to `ares-test-support` — the single shared resolver for
//! every crate's live tests. URL resolution, lifecycle (migrate + truncate
//! before, truncate after), and loud failure with fix instructions all live
//! there.

use ares_store::PostgresClient;

/// Returns the test database URL.
///
/// Priority: `TEST_DATABASE_URL`, then `DATABASE_URL` (postgres only,
/// rewritten to `ares_test`), then unix-socket peer auth. See
/// `ares_test_support::test_db_url`.
pub fn test_db_url() -> String {
    ares_test_support::test_db_url()
}

/// Connect to the test DB. On first call per binary, runs cleanup + migrations.
/// Subsequent calls share the pool.
pub async fn create_test_db() -> PostgresClient {
    ares_test_support::client().await
}
