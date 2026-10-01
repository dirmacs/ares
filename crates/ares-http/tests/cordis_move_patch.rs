//! cordis-move-fix: a cordis `PATCH` applies its field updates to the entry
//! the request addressed (under its new id after a move), and its
//! `admin_audit_log` row names that entry and no other target.
//!
//! `Loader::move_entry` lists the moved entry first, then its descendants.
//! From `5428514` the handler applied the fields to the LAST id of that list,
//! a descendant when the moved entry has children; 1.16-FIX-3 (`974b296`)
//! pointed the row at the moved entry and recorded the descendant as
//! `fields_applied_to`. With the fix the fields land on the entry the row
//! names, so no row carries `fields_applied_to`.
//!
//! Where the fields land, with the live fibers, is tested without a database
//! in the handler's own test module (`patch_target_*`). This file pins the
//! row, which needs a live Postgres: the gate is `audit_writes_live.rs`'s
//! (`TEST_DATABASE_URL` set and unreachable panics; unset skips by the
//! crate's convention, `tests/common/mod.rs`).

#![cfg(feature = "postgres")]

mod common;

use std::sync::{Arc, Mutex};

use ares_http::api::handlers::admin::cordis::patch_cordis_entry;
use ares_http::api::handlers::admin::AdminActor;
use ares_store::TenantDb;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use cordis::loader::{EntryTree, EntryUpdate};
use cordis::Context;
use sqlx::PgPool;
use sqlx::Row;

/// A fresh root `Context` carrying a real, connected `TenantDb`, plus the pool
/// underneath it. `None` only for the unconfigured skip.
async fn live_ctx() -> Option<(Arc<Context>, PgPool)> {
    common::live_db_url(&common::current_test_name()).await?;
    let pg = ares_test_support::client().await;
    let pool = pg.pool.clone();
    let ctx = Context::new_root();
    ctx.provide_arc(Arc::new(TenantDb::new(Arc::new(pg))));
    Some((ctx, pool))
}

fn admin_actor() -> AdminActor {
    AdminActor {
        subject: Some("cordis-move-fix-test-admin".to_string()),
        email: None,
        auth: Some("jwt"),
        client_ip: Some("203.0.113.98".to_string()),
    }
}

/// A temp cordis program directory, removed on drop even when an assertion
/// fails first.
struct TempEntriesDir(std::path::PathBuf);

impl Drop for TempEntriesDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl TempEntriesDir {
    fn program(&self) -> std::path::PathBuf {
        self.0.join("cordis-entries.toml")
    }
}

/// Loader state on `ctx` (journal, registry, `CurrentEntries`) over a program
/// file holding `entries`, each `(id, plugin, parent)`. The file is also the
/// applied tree, as after boot.
fn seed_entries(ctx: &Arc<Context>, entries: &[(&str, &str, Option<&str>)]) -> TempEntriesDir {
    ctx.provide(cordis::ReflectService::new());
    cordis::LoaderJournal::provide_new(ctx);
    ctx.provide(cordis::RegistryService::new());
    let dir = TempEntriesDir(
        std::env::temp_dir().join(format!("cordis-move-fix-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir_all(&dir.0).expect("temp entries dir");
    let mut program = String::new();
    for (id, plugin, parent) in entries {
        program.push_str(&format!(
            "[[entry]]\nid = \"{id}\"\nplugin = \"{plugin}\"\ndisabled = false\n\n[entry.config]\n\n"
        ));
        if let Some(parent) = parent {
            program.push_str(&format!(
                "[entry.position]\nparent = \"{parent}\"\nposition = 0\n\n"
            ));
        }
    }
    std::fs::write(dir.program(), program).expect("seed entries file");
    let tree =
        cordis::loader::Loader::load_from_file(&dir.program()).expect("parse the seeded entries");
    ctx.provide_arc(Arc::new(cordis::CurrentEntries {
        tree: Arc::new(Mutex::new(tree)),
        path: dir.program(),
    }));
    dir
}

/// `disabled` of the entry `id` in `tree`; `None` when no entry has that id.
fn disabled_of(tree: &EntryTree, id: &str) -> Option<bool> {
    tree.0.iter().find(|e| e.id == id).map(|e| e.disabled)
}

/// The `details` of the one `patch_cordis_entry` row under `resource_id`.
/// Panics unless exactly one row has that id, with that action.
async fn patch_row_details(pool: &PgPool, resource_id: &str) -> serde_json::Value {
    let rows = sqlx::query(
        "SELECT action, resource_type, actor, details FROM admin_audit_log \
         WHERE resource_id = $1",
    )
    .bind(resource_id)
    .fetch_all(pool)
    .await
    .expect("audit rows query");
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one audit row under {resource_id}"
    );
    let row = &rows[0];
    assert_eq!(row.get::<String, _>("action"), "patch_cordis_entry");
    assert_eq!(row.get::<String, _>("resource_type"), "cordis_entry");
    assert_eq!(
        row.get::<Option<String>, _>("actor").as_deref(),
        Some("cordis-move-fix-test-admin")
    );
    let details: Option<String> = row.get("details");
    serde_json::from_str(details.as_deref().expect("details")).expect("json details")
}

/// No audit row has `resource_id`.
async fn assert_no_row(pool: &PgPool, resource_id: &str) {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM admin_audit_log WHERE resource_id = $1")
            .bind(resource_id)
            .fetch_one(pool)
            .await
            .expect("audit count query");
    assert_eq!(count, 0, "no row for this request under {resource_id}");
}

/// The red case: `PATCH grp {parent: top, disabled: true}` with `grp` holding
/// a child. The row names `top:grp`, its `details` name no other target, and
/// the saved program agrees: `top:grp` disabled, `top:grp:svc` not.
#[tokio::test]
async fn patch_target_audit_move_with_children_names_no_other_target() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let top = format!("top{}", uuid::Uuid::new_v4().simple());
    let grp = format!("grp{}", uuid::Uuid::new_v4().simple());
    let child = format!("{grp}:svc");
    let dir = seed_entries(
        &ctx,
        &[
            (top.as_str(), "GroupMarker", None),
            (grp.as_str(), "GroupMarker", None),
            (child.as_str(), "CalculatorService", None),
        ],
    );

    let (status, Json(body)) = patch_cordis_entry(
        State(ctx.clone()),
        admin_actor(),
        Path(grp.clone()),
        Json(EntryUpdate {
            parent: Some(Some(top.clone())),
            disabled: Some(true),
            ..Default::default()
        }),
    )
    .await
    .expect("patch_cordis_entry response");
    assert_eq!(status, StatusCode::OK, "{body}");

    let moved = format!("{top}:{grp}");
    let moved_child = format!("{moved}:svc");
    assert_eq!(
        patch_row_details(&pool, &moved).await,
        serde_json::json!({ "fields": ["disabled", "parent"], "previous_id": grp }),
        "the row names the moved entry and no other target"
    );
    assert_no_row(&pool, &moved_child).await;

    let saved = cordis::loader::Loader::load_from_file(&dir.program()).expect("saved program");
    assert_eq!(disabled_of(&saved, &moved), Some(true), "{saved:?}");
    assert_eq!(disabled_of(&saved, &moved_child), Some(false), "{saved:?}");
}

/// The reorder variant: `PATCH top:grp {position: 0, disabled: true}` with
/// `top:grp` under `top` and holding a child. No id changes, so the row has
/// no `previous_id`, and no other target either.
#[tokio::test]
async fn patch_target_audit_reorder_with_children_names_no_other_target() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let top = format!("top{}", uuid::Uuid::new_v4().simple());
    let grp = format!("{top}:grp");
    let child = format!("{grp}:svc");
    let dir = seed_entries(
        &ctx,
        &[
            (top.as_str(), "GroupMarker", None),
            (grp.as_str(), "GroupMarker", Some(top.as_str())),
            (child.as_str(), "CalculatorService", Some(grp.as_str())),
        ],
    );

    let (status, Json(body)) = patch_cordis_entry(
        State(ctx.clone()),
        admin_actor(),
        Path(grp.clone()),
        Json(EntryUpdate {
            position: Some(0),
            disabled: Some(true),
            ..Default::default()
        }),
    )
    .await
    .expect("patch_cordis_entry response");
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(
        patch_row_details(&pool, &grp).await,
        serde_json::json!({ "fields": ["disabled", "position"], "previous_id": null }),
        "the row names the reordered entry and no other target"
    );
    assert_no_row(&pool, &child).await;

    let saved = cordis::loader::Loader::load_from_file(&dir.program()).expect("saved program");
    assert_eq!(disabled_of(&saved, &grp), Some(true), "{saved:?}");
    assert_eq!(disabled_of(&saved, &child), Some(false), "{saved:?}");
}

/// Control: a move of a leaf. The row and the fields were already on the
/// moved entry; unchanged.
#[tokio::test]
async fn patch_target_audit_leaf_move_unchanged() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let top = format!("top{}", uuid::Uuid::new_v4().simple());
    let leaf = format!("leaf{}", uuid::Uuid::new_v4().simple());
    let dir = seed_entries(
        &ctx,
        &[
            (top.as_str(), "GroupMarker", None),
            (leaf.as_str(), "CalculatorService", None),
        ],
    );

    let (status, Json(body)) = patch_cordis_entry(
        State(ctx.clone()),
        admin_actor(),
        Path(leaf.clone()),
        Json(EntryUpdate {
            parent: Some(Some(top.clone())),
            disabled: Some(true),
            ..Default::default()
        }),
    )
    .await
    .expect("patch_cordis_entry response");
    assert_eq!(status, StatusCode::OK, "{body}");

    let moved = format!("{top}:{leaf}");
    assert_eq!(
        patch_row_details(&pool, &moved).await,
        serde_json::json!({ "fields": ["disabled", "parent"], "previous_id": leaf }),
    );
    let saved = cordis::loader::Loader::load_from_file(&dir.program()).expect("saved program");
    assert_eq!(disabled_of(&saved, &moved), Some(true), "{saved:?}");
    assert_eq!(disabled_of(&saved, &top), Some(false), "{saved:?}");
}

/// Control: a PATCH with no move on an entry with a child. The row names the
/// entry, with no `previous_id`; unchanged.
#[tokio::test]
async fn patch_target_audit_no_move_unchanged() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let grp = format!("grp{}", uuid::Uuid::new_v4().simple());
    let child = format!("{grp}:svc");
    let dir = seed_entries(
        &ctx,
        &[
            (grp.as_str(), "GroupMarker", None),
            (child.as_str(), "CalculatorService", None),
        ],
    );

    let (status, Json(body)) = patch_cordis_entry(
        State(ctx.clone()),
        admin_actor(),
        Path(grp.clone()),
        Json(EntryUpdate {
            disabled: Some(true),
            ..Default::default()
        }),
    )
    .await
    .expect("patch_cordis_entry response");
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(
        patch_row_details(&pool, &grp).await,
        serde_json::json!({ "fields": ["disabled"], "previous_id": null }),
    );
    assert_no_row(&pool, &child).await;
    let saved = cordis::loader::Loader::load_from_file(&dir.program()).expect("saved program");
    assert_eq!(disabled_of(&saved, &grp), Some(true), "{saved:?}");
    assert_eq!(disabled_of(&saved, &child), Some(false), "{saved:?}");
}
