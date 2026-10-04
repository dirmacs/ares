//! Per-tenant pause switch.
//!
//! Scope note: these cover the store contract — the flag, its defaults, and
//! attribution. The run-path gate itself lives in
//! `crates/ares-http/src/api/handlers/v1/agents.rs`, and the "a paused run makes
//! no model call" assertion belongs to an HTTP-level test that drives
//! `POST /v1/agents/{name}/run` and then reads `run_llm_calls`. That end-to-end
//! test is NOT in this file; the PR description states what is and is not
//! covered.
//!
//! Each test names its own tenant, so it neither depends on nor disturbs any
//! pre-existing rows. `create_tenant` takes a *name* and mints the id, so the
//! id used below is the one it returns rather than one we chose.

mod common;

use std::sync::Arc;

use ares_store::TenantDb;
use common::test_db::create_test_db;

/// The harness hands back a `PostgresClient`; the tenant API hangs off
/// `TenantDb`, which wraps it.
async fn test_db() -> TenantDb {
    TenantDb::new(Arc::new(create_test_db().await))
}

/// Creates a tenant with a unique name and returns its store handle plus id.
async fn fresh_tenant(db: &TenantDb, tag: &str) -> String {
    let name = format!("pause-test-{}-{}", tag, uuid::Uuid::new_v4());
    db.create_tenant(name, ares_types::TenantTier::Free)
        .await
        .expect("tenant should be created")
        .id
}

#[tokio::test]
async fn existing_tenant_defaults_to_not_paused() {
    let db = test_db().await;
    let id = fresh_tenant(&db, "default").await;

    let flags = db.get_tenant_flags(&id).await.expect("flags should read");
    assert!(
        !flags.paused,
        "a tenant created after the migration must default to not paused"
    );
    assert!(
        flags.paused_by.is_none(),
        "an unpaused tenant must carry no attribution"
    );
    assert!(
        !flags.strict,
        "strict is added by this migration but its behaviour is out of scope; default false"
    );
}

#[tokio::test]
async fn pausing_records_the_actor() {
    let db = test_db().await;
    let id = fresh_tenant(&db, "actor").await;

    db.set_tenant_paused(&id, true, "operator@example.com")
        .await
        .expect("pause should succeed");

    let flags = db.get_tenant_flags(&id).await.expect("flags should read");
    assert!(flags.paused, "the tenant must report paused");
    assert_eq!(
        flags.paused_by.as_deref(),
        Some("operator@example.com"),
        "the pause must be attributable to whoever performed it"
    );
}

#[tokio::test]
async fn unpausing_clears_the_attribution() {
    let db = test_db().await;
    let id = fresh_tenant(&db, "clear").await;

    db.set_tenant_paused(&id, true, "operator@example.com")
        .await
        .expect("pause should succeed");
    db.set_tenant_paused(&id, false, "operator@example.com")
        .await
        .expect("unpause should succeed");

    let flags = db.get_tenant_flags(&id).await.expect("flags should read");
    assert!(!flags.paused, "the tenant must report not paused");
    assert!(
        flags.paused_by.is_none(),
        "clearing the pause must clear the actor too, or an unpaused tenant keeps a stale one"
    );
}

#[tokio::test]
async fn pausing_one_tenant_does_not_touch_another() {
    let db = test_db().await;
    let paused_id = fresh_tenant(&db, "iso-paused").await;
    let other_id = fresh_tenant(&db, "iso-other").await;

    db.set_tenant_paused(&paused_id, true, "operator@example.com")
        .await
        .expect("pause should succeed");

    let paused = db.get_tenant_flags(&paused_id).await.expect("read");
    let other = db.get_tenant_flags(&other_id).await.expect("read");
    assert!(paused.paused, "the paused tenant is paused");
    assert!(
        !other.paused,
        "pausing one tenant must leave every other tenant running"
    );
}

#[tokio::test]
async fn pausing_an_unknown_tenant_is_an_error_not_a_silent_no_op() {
    let db = test_db().await;
    let missing = format!("no-such-tenant-{}", uuid::Uuid::new_v4());

    let err = db
        .set_tenant_paused(&missing, true, "operator@example.com")
        .await
        .expect_err("pausing a tenant that does not exist must not report success");
    assert!(
        matches!(err, ares_types::AppError::NotFound(_)),
        "expected NotFound, got {:?}",
        err
    );
}

#[tokio::test]
async fn pause_history_survives_unpausing() {
    let db = test_db().await;
    let id = fresh_tenant(&db, "history").await;

    db.set_tenant_paused(&id, true, "alice@ops")
        .await
        .expect("pause should succeed");
    db.set_tenant_paused(&id, false, "bob@ops")
        .await
        .expect("unpause should succeed");

    // The tenant row itself can no longer answer who paused: unpausing cleared
    // the attribution by design.
    let flags = db.get_tenant_flags(&id).await.expect("flags should read");
    assert!(
        flags.paused_by.is_none(),
        "the tenant row is expected to forget the actor once unpaused"
    );

    // The audit trail is what must survive.
    let rows: Vec<(bool, String)> = sqlx::query_as(
        "SELECT paused, actor FROM tenant_pause_audit WHERE tenant_id = $1 ORDER BY id",
    )
    .bind(&id)
    .fetch_all(db.pool())
    .await
    .expect("audit rows should read");

    assert_eq!(
        rows.len(),
        2,
        "both the pause and the unpause must leave a row, not just the current state"
    );
    assert_eq!(rows[0], (true, "alice@ops".to_string()));
    assert_eq!(rows[1], (false, "bob@ops".to_string()));
}
