//! Test-only helpers for the live-Postgres tests.
//!
//! Every `#[cfg(test)]` integration test in this crate talks to the same
//! `ares_test` database, and libtest runs them in parallel. Several tests
//! clean up rows by name prefix, so a sibling test can lose the fixtures it is
//! asserting on. [`pool`] serialises the DB-using bodies: it takes a
//! session-level Postgres advisory lock for the caller's whole test. The lock
//! works across threads and across test binaries. Pure unit tests stay fully
//! parallel.

use sqlx::{Connection, PgConnection, PgPool};

/// Fixed advisory-lock key shared by every live-DB test in this crate.
///
/// Arbitrary value; hex spells `ares_sto`.
const TEST_LOCK_KEY: i64 = 0x6172_6573_5F73_746F;

/// Guard that holds the crate-wide test lock until it is dropped.
pub struct TestLock {
    /// Dedicated session that owns the session-level advisory lock. The lock
    /// releases when this session closes on drop.
    _session: PgConnection,
}

/// Connect the shared live-test pool, then take the crate-wide test lock.
///
/// Keep the returned guard alive for the whole test body.
pub async fn pool() -> (TestLock, PgPool) {
    let pool = ares_test_support::pool().await;
    let mut session = PgConnection::connect(&ares_test_support::test_db_url())
        .await
        .expect("connect test DB lock session");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(TEST_LOCK_KEY)
        .execute(&mut session)
        .await
        .expect("take test DB advisory lock");
    (TestLock { _session: session }, pool)
}
