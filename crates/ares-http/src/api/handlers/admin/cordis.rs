//! Cordis service-lifecycle domain — runtime retire / re-provide of services.
//!
//! Complements the reactive-fiber demo wired in `run_server`: retiring
//! `EventsService` flips dependent fibers (demo fid 990001) to `Inactive`;
//! re-providing flips them back to `Active`. Only DIRECT-provided concrete
//! types are retirably supported today; wrapper services
//! (`crate::context_services::*Service`) hold an inner `Arc<T>` under a
//! distinct TypeId, so removal would not cascade — those answer 409.

use crate::HttpError;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use ::cordis::{Context, EventsService, ReflectService};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};

type RetireFn = fn(&Arc<Context>) -> Option<String>;

/// Static registry of retirable services, keyed by wire name.
///
/// Entries map a name to a plain fn performing the real
/// `Context::remove::<T>()` (which notifies dependents via `ReflectService`
/// BFS and pushes a re-provide undo onto the fiber accumulator). Wrapper
/// services are deliberately absent from this registry.
static RETIRE_MAP: LazyLock<RwLock<HashMap<String, RetireFn>>> = LazyLock::new(|| {
    RwLock::new(HashMap::from([(
        "events_service".to_string(),
        (|ctx: &Arc<Context>| {
            ctx.remove::<EventsService>()
                .map(|_| std::any::type_name::<EventsService>().to_string())
        }) as RetireFn,
    )]))
});

/// POST /admin/cordis/services/:name/retire — runtime-remove a service instance.
///
/// Removal is the real `Context::remove::<T>`: store entry dropped by
/// `TypeId`, version bumped down, dependents notified (BFS → `Fiber::refresh`)
/// and a LIFO undo pushed onto the fiber accumulator. Responds
/// `200 {"retired": true, ...}` on removal, `200 {"retired": false, ...}`
/// when the service was already absent, and `409` for names that are not
/// direct Cordis services (wrapper-registered types are not supported today).
pub async fn retire_cordis_service(
    State(ctx): State<Arc<Context>>,
    Path(name): Path<String>,
) -> crate::Result<(StatusCode, Json<serde_json::Value>)> {
    let retire = {
        let map = RETIRE_MAP.read().expect("RETIRE_MAP poisoned");
        map.get(name.as_str()).copied()
    };

    let Some(retire) = retire else {
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "retired": false,
                "service": name,
                "error": format!(
                    "service {name} is not retirably supported: only direct Cordis \
                     services (concrete-type provides) can be retired; wrapper \
                     services are not supported today"
                ),
                "cascaded_notify": ctx.get::<ReflectService>().is_some(),
            })),
        ));
    };

    let removed_type = retire(&ctx);
    // `Context::remove` already notified dependents (reflect.notify(tid));
    // a second explicit `notify` is skipped to avoid duplicate fan-out.
    let cascaded_notify = ctx.get::<ReflectService>().is_some();
    tracing::info!(
        service = %name,
        removed = removed_type.is_some(),
        cascaded_notify,
        "cordis service retire requested via admin API"
    );
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "retired": removed_type.is_some(),
            "service": name,
            "removed_type": removed_type,
            "cascaded_notify": cascaded_notify,
        })),
    ))
}

/// POST /admin/cordis/services/:name/provide — re-register a retirable service
/// so retire/provide cycles are demonstrable repeatedly.
pub async fn provide_cordis_service(
    State(ctx): State<Arc<Context>>,
    Path(name): Path<String>,
) -> crate::Result<(StatusCode, Json<serde_json::Value>)> {
    match name.as_str() {
        "events_service" => {
            if ctx.get::<EventsService>().is_some() {
                return Ok((
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "provided": false,
                        "service": name,
                        "reason": "already present",
                    })),
                ));
            }
            ctx.provide(EventsService::new());
            tracing::info!(service = %name, "cordis service re-provided via admin API");
            Ok((
                StatusCode::OK,
                Json(serde_json::json!({
                    "provided": true,
                    "service": name,
                    "type": std::any::type_name::<EventsService>(),
                })),
            ))
        }
        other => Err(HttpError::from(ares_types::types::AppError::InvalidInput(format!(
            "service {other} cannot be provided dynamically: only direct Cordis \
             services with known constructors are supported today"
        )))),
    }
}

pub fn routes() -> axum::Router<Arc<Context>> {
    use axum::routing::post;
    axum::Router::new()
        .route("/cordis/services/{name}/retire", post(retire_cordis_service))
        .route("/cordis/services/{name}/provide", post(provide_cordis_service))
}

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ::cordis::Service;


/// POST /admin/cordis/entries/reload — reload `cordis-entries.toml` through
/// `Loader::reload_current`, diffing against the boot-applied tree. Shares the
/// same journal/current-tree state as the file watcher. Returns per-action
/// outcomes; 503 when loader state is missing (library deployments without a
/// file program).
pub async fn reload_cordis_entries(
    State(ctx): State<Arc<Context>>,
) -> crate::Result<(StatusCode, Json<serde_json::Value>)> {
    use cordis::loader::Loader;

    let Some(journal) = ctx.get::<cordis::LoaderJournal>() else {
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "reloaded": false,
                "error": "LoaderJournal is not provided on this context",
            })),
        ));
    };
    let Some(current_entries) = ctx.get::<cordis::CurrentEntries>() else {
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "reloaded": false,
                "error": "CurrentEntries is not provided on this context",
            })),
        ));
    };

    let mut current = current_entries
        .tree
        .lock()
        .expect("entries lock")
        .clone();
    let actions = Loader::reload_current(
        &ctx,
        &current_entries.path,
        &mut current,
        &journal,
    )
    .await;

    let Some(actions) = actions else {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "reloaded": false,
                "error": format!(
                    "failed to read or parse {}",
                    current_entries.path.display()
                ),
            })),
        ));
    };

    let applied: Vec<serde_json::Value> = actions
        .iter()
        .map(|a| match &a.status {
            Ok(()) => serde_json::json!({
                "id": a.id, "action": a.action, "ok": true,
            }),
            Err(err) => serde_json::json!({
                "id": a.id, "action": a.action, "ok": false, "error": err,
            }),
        })
        .collect();
    *current_entries.tree.lock().expect("entries lock") = current;
    Ok((StatusCode::OK, Json(serde_json::json!({ "applied": applied }))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn retire_removes_events_service_by_type_id_and_provide_restores_it() {
        let ctx = Context::new_root();
        ctx.provide(ReflectService::new());
        ctx.provide(EventsService::new());
        assert!(ctx.get::<EventsService>().is_some());

        // Wrapper / unsupported names answer 409 Conflict.
        let resp = retire_cordis_service(State(ctx.clone()), Path("tool_registry".into()))
            .await
            .expect("handler");
        assert_eq!(resp.0, StatusCode::CONFLICT);
        assert_eq!(resp.1 .0["retired"], json!(false));

        // Real retirement: store entry dropped by TypeId.
        let resp = retire_cordis_service(State(ctx.clone()), Path("events_service".into()))
            .await
            .expect("handler");
        assert_eq!(resp.0, StatusCode::OK);
        assert_eq!(resp.1 .0["retired"], json!(true));
        assert_eq!(
            resp.1 .0["removed_type"],
            json!(std::any::type_name::<EventsService>())
        );
        assert_eq!(resp.1 .0["cascaded_notify"], json!(true));
        assert!(ctx.get::<EventsService>().is_none());

        // Retiring again reports already-absent, still 200.
        let resp = retire_cordis_service(State(ctx.clone()), Path("events_service".into()))
            .await
            .expect("handler");
        assert_eq!(resp.0, StatusCode::OK);
        assert_eq!(resp.1 .0["retired"], json!(false));

        // Re-provide flips dependent fibers back on; cycle repeatable.
        let resp = provide_cordis_service(State(ctx.clone()), Path("events_service".into()))
            .await
            .expect("handler");
        assert_eq!(resp.0, StatusCode::OK);
        assert_eq!(resp.1 .0["provided"], json!(true));
        assert!(ctx.get::<EventsService>().is_some());

        // Providing while present reports already-present.
        let resp = provide_cordis_service(State(ctx.clone()), Path("events_service".into()))
            .await
            .expect("handler");
        assert_eq!(resp.1 .0["provided"], json!(false));

        // Unknown constructors are rejected as invalid input.
        assert!(provide_cordis_service(State(ctx.clone()), Path("nope".into()))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn retire_notifies_dependent_fiber_via_reflect_bfs() {
        use std::any::TypeId;

        let ctx = Context::new_root();
        let reflect = ctx.provide(ReflectService::new());
        reflect.set_context(&ctx);
        reflect.ensure_notifier_for::<EventsService>();
        ctx.provide(EventsService::new());

        let fiber = Arc::new(::cordis::Fiber::new());
        fiber.declare_inject::<EventsService>();
        let fid: u64 = 990_002;
        reflect.register_dependent(TypeId::of::<EventsService>(), fid);
        reflect.register_fiber(fid, fiber.clone(), TypeId::of::<EventsService>());
        fiber.refresh(&ctx).await;
        assert!(matches!(fiber.state(), ::cordis::FiberState::Active { .. }));

        retire_cordis_service(State(ctx.clone()), Path("events_service".into()))
            .await
            .expect("handler");
        // remove() notified dependents; give the spawned refresh a beat.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(matches!(
            fiber.state(),
            ::cordis::FiberState::Inactive { .. }
        ));

        provide_cordis_service(State(ctx.clone()), Path("events_service".into()))
            .await
            .expect("handler");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(matches!(fiber.state(), ::cordis::FiberState::Active { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reload_endpoint_applies_add_and_retire_from_file() {
        use cordis::loader::{Entry, EntryTree, Loader};
        let loader = Loader;

        let ctx = Context::new_root();
        ctx.provide(ReflectService::new());
        cordis::LoaderJournal::provide_new(&ctx);
        ctx.provide(cordis::RegistryService::new());
        // Minimal plugin factory the reload can instantiate.
        let registry = ctx.provide(cordis::PluginRegistry::new());

        #[derive(Debug)]
        struct Probe(u64);
        impl Service for Probe {}
        registry.register(
            "CalculatorService",
            Arc::new(|ctx, _cfg| {
                let fut = ctx.plugin(Probe(0));
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(fut)
                })
            }),
        );

        let dir = std::env::temp_dir().join(format!(
            "cordis-reload-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("tmpdir");
        let path = dir.join("cordis-entries.toml");
        std::fs::write(&path, "").expect("empty entries file");

        // Seed: boot applied an empty tree.
        let seed = cordis::CurrentEntries {
            tree: Arc::new(std::sync::Mutex::new(EntryTree(vec![]))),
            path: path.clone(),
        };
        ctx.provide_arc(Arc::new(seed));
        let current_entries = ctx.get::<cordis::CurrentEntries>().unwrap();

        // 1) add a calculator entry → Begin-ok
        std::fs::write(
            &path,
            "[[entry]]\nid = \"calc\"\nplugin = \"CalculatorService\"\ndisabled = false\n\n[entry.config]\n",
        )
        .expect("write entries v2");
        let (status, Json(body)) =
            reload_cordis_entries(State(ctx.clone())).await.expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["applied"][0]["action"], "begin");
        assert_eq!(body["applied"][0]["ok"], true);
        assert!(ctx.get::<Probe>().is_some(), "calculator instantiated");

        // 2) remove it → Retire-ok
        std::fs::write(&path, "").expect("write entries v3");
        let (status, Json(body)) =
            reload_cordis_entries(State(ctx.clone())).await.expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["applied"][0]["action"], "retire");
        assert_eq!(body["applied"][0]["ok"], true);
        assert!(ctx.get::<Probe>().is_none(), "retired service removed");

        // current tree tracks desired across both applies
        {
            let tree = current_entries.tree.lock().unwrap();
            assert_eq!(tree.0.len(), 0);
            assert_eq!(
                Loader.reconcile(&tree, &EntryTree(vec![Entry {
                    id: "calc".into(),
                    plugin: "CalculatorService".into(),
                    config: serde_json::Value::Null,
                    disabled: false,
                    isolate: None,
                    intercept: Default::default(),
                }])).len(),
                1,
                "sanity: diff detects a re-add"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
