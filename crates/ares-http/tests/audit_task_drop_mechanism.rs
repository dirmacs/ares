//! Diagnosis evidence for item 1.16 (VERIFY-2026-09-22.md §4 row 20): isolates the mechanism by
//! which a `tokio::spawn`ed, discarded audit insert is lost, independent of
//! any specific handler. This is the shape every base-commit call site used
//! (`tokio::spawn(async move { let _ = log_admin_action(...).await; })`)
//! before this item's fix — kept alive here as a standalone reproduction so
//! the mechanism stays demonstrated even though no production call site uses
//! it anymore.
//!
//! Mechanism shown: a task hint into `tokio::spawn` with its `JoinHandle`
//! immediately dropped (fire-and-forget) is scheduled on the runtime but not
//! guaranteed to run to completion before that runtime is torn down. A
//! `#[tokio::test]` gets its own per-test runtime that is dropped the moment
//! the test function returns (this is `tokio`'s documented behaviour, not
//! specific to this codebase) — the same event class as a process exit
//! (deploy, restart, crash) in production. Neither this test's assertion nor
//! anything else observes whether the spawned task ran; the row is simply
//! not there, with no error anywhere.

#![cfg(feature = "postgres")]

use std::time::Duration;

async fn db_reachable(url: &str) -> bool {
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

/// Spawns a task in the *old* shape (detached, discarded `JoinHandle`,
/// discarded inner `Result`) that inserts one row, then returns immediately
/// — matching a handler that returns its HTTP response without awaiting the
/// audit write. The calling test's runtime tears down right after this
/// function returns, before the spawned task has necessarily run.
async fn spawn_and_discard_like_the_old_code(pool: sqlx::PgPool, marker: String) {
    tokio::spawn(async move {
        let _ = sqlx::query("INSERT INTO audit_task_drop_probe (marker) VALUES ($1)")
            .bind(&marker)
            .execute(&pool)
            .await;
    });
    // No `.await` on the JoinHandle, no sleep: this function returns the
    // instant the spawn call is made, exactly like the old handlers
    // returning their HTTP response without waiting on the audit write.
}

#[tokio::test]
async fn task_dropped_at_runtime_teardown_loses_the_pending_insert() {
    let url = ares_test_support::test_db_url();
    if !db_reachable(&url).await {
        eprintln!("SKIPPED: test database unreachable ({url})");
        return;
    }
    let pool = ares_test_support::pool().await;

    sqlx::query("CREATE TABLE IF NOT EXISTS audit_task_drop_probe (marker text primary key)")
        .execute(&pool)
        .await
        .expect("create probe table");

    let marker = format!("probe-{}", uuid::Uuid::new_v4());

    // This is the entire scope of a `#[tokio::test]`'s runtime: the spawn
    // happens, the function returns, and — because this is a *nested* async
    // call inside the test body rather than the test's own top-level future
    // — the outer `#[tokio::test]` runtime is what tears down. To reproduce
    // teardown happening WHILE the spawned task is still pending (not merely
    // "hasn't been polled yet, but the runtime is still alive and will get
    // to it"), drive the spawn+return on a throwaway runtime that is dropped
    // synchronously the instant `block_on` returns — the same event class as
    // process exit.
    let marker_for_thread = marker.clone();
    let pool_for_thread = pool.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build throwaway runtime");
        rt.block_on(spawn_and_discard_like_the_old_code(
            pool_for_thread,
            marker_for_thread,
        ));
        // `rt` drops here, synchronously, immediately after `block_on`
        // returns — before the scheduler has any guarantee of having polled
        // the detached task to completion.
    })
    .join()
    .expect("throwaway runtime thread");

    // Back on the *main* test runtime (a different, still-alive runtime and
    // connection pool), check immediately: no sleep, no retry.
    let row: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM audit_task_drop_probe WHERE marker = $1")
            .bind(&marker)
            .fetch_one(&pool)
            .await
            .expect("count query");
    assert_eq!(
        row.0, 0,
        "the detached insert must have been lost when its runtime tore down \
         before it was polled to completion — if this fails, the mechanism \
         did not reproduce on this run (a scheduling race, not a fix)"
    );

    // Cleanup: harmless if the row never landed.
    let _ = sqlx::query("DELETE FROM audit_task_drop_probe WHERE marker = $1")
        .bind(&marker)
        .execute(&pool)
        .await;
}
