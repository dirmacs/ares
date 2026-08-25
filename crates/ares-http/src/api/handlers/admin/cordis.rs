//! Cordis service-lifecycle domain — runtime retire / re-provide / hot
//! provider replacement of services.
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

use ::cordis::{Context, EventsService, ReflectService, RegistryService};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};

/// Outcome of one retirability probe.
struct RetireOutcome {
    /// Debug type name when the service instance was actually removed.
    removed_type: Option<String>,
    /// Set when guarded withdrawal refused the removal: the number of active
    /// consumer fibers still relying on the provider.
    guarded: Option<usize>,
}

type RetireFn = fn(&Arc<Context>) -> RetireOutcome;

/// Static registry of retirable services, keyed by wire name.
///
/// Entries map a name to a plain fn performing the real
/// `Context::remove::<T>()` (which notifies dependents via `ReflectService`
/// BFS and pushes a re-provide undo onto the fiber accumulator). Removal is
/// guarded: active consumer fibers block the withdrawal (paper §4.3.1).
/// Wrapper services are deliberately absent from this registry.
static RETIRE_MAP: LazyLock<RwLock<HashMap<String, RetireFn>>> = LazyLock::new(|| {
    RwLock::new(HashMap::from([(
        "events_service".to_string(),
        (|ctx: &Arc<Context>| match ctx.remove::<EventsService>() {
            Ok(removed) => RetireOutcome {
                removed_type: removed.map(|_| std::any::type_name::<EventsService>().to_string()),
                guarded: None,
            },
            Err(err) if err.to_string().contains("guarded withdrawal") => {
                let tid = std::any::TypeId::of::<EventsService>();
                let consumers = ctx
                    .get::<RegistryService>()
                    .map(|rs| rs.reliance_count(&(tid, ctx.isolate_label(tid))))
                    .unwrap_or(0);
                tracing::info!(
                    consumers,
                    "cordis retire refused: guarded withdrawal with active consumers"
                );
                RetireOutcome {
                    removed_type: None,
                    guarded: Some(consumers),
                }
            }
            Err(_) => RetireOutcome {
                removed_type: None,
                guarded: None,
            },
        }) as RetireFn,
    )]))
});

/// POST /admin/cordis/services/:name/retire — runtime-remove a service instance.
///
/// Removal is the real `Context::remove::<T>`: store entry dropped by
/// `TypeId`, version bumped down, dependents notified (BFS → `Fiber::refresh`)
/// and a LIFO undo pushed onto the fiber accumulator. Responds
/// `200 {"retired": true, ...}` on removal, `200 {"retired": false, ...}`
/// when the service was already absent, `409 {"retired": false,
/// "reason": "guarded", "consumers": N}` when active consumer fibers still
/// rely on the provider (guarded withdrawal), and `409` for names that are
/// not direct Cordis services (wrapper types are not supported today).
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

    let outcome = retire(&ctx);
    if let Some(consumers) = outcome.guarded {
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "retired": false,
                "service": name,
                "reason": "guarded",
                "consumers": consumers,
                "cascaded_notify": ctx.get::<ReflectService>().is_some(),
            })),
        ));
    }
    let removed_type = outcome.removed_type;
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
        other => Err(HttpError::from(ares_types::types::AppError::InvalidInput(
            format!(
                "service {other} cannot be provided dynamically: only direct Cordis \
             services with known constructors are supported today"
            ),
        ))),
    }
}

/// POST /admin/cordis/services/:name/replace — rolling drain-and-shift
/// replacement of a journaled provider's configuration (zero absence window).
///
/// Body must be `{"config": <value>}` carrying the NEW configuration for the
/// plugin's registered factory; anything else answers 400 naming that
/// requirement. Execution goes through
/// [`cordis::loader::Loader::replace_provider`] with the shared
/// `LoaderJournal`: the candidate is built on a scratch child context and
/// verified before bridge → dispose-old → promote swaps it in, so consumers
/// observe no resolution gap. A refusal (unknown plugin label, untracked /
/// isolated provider, failing trial) leaves the old provider serving
/// untouched BY DESIGN and answers `409 {"replaced": false, "reason": msg}`;
/// success answers `200 {"replaced": true, "plugin", "fiber_id"}` with the
/// fresh registration fiber id. 503 mirrors the sibling endpoints when the
/// loader state (journal) is absent on this context.
pub async fn replace_cordis_service(
    State(ctx): State<Arc<Context>>,
    Path(name): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> crate::Result<(StatusCode, Json<serde_json::Value>)> {
    use cordis::loader::Loader;

    let Some(config) = body.get("config") else {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "replaced": false,
                "error": format!(
                    "body must be {{\"config\": <value>}} carrying the new \
                     provider configuration; got {body}"
                ),
            })),
        ));
    };
    let Some(journal) = ctx.get::<cordis::LoaderJournal>() else {
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "replaced": false,
                "error": "LoaderJournal is not provided on this context",
            })),
        ));
    };

    let loader = Loader;
    match loader
        .replace_provider(&ctx, &name, config.clone(), &journal)
        .await
    {
        Ok(fiber_id) => {
            tracing::info!(
                service = %name,
                fiber_id,
                "cordis provider replaced via admin API"
            );
            Ok((
                StatusCode::OK,
                Json(serde_json::json!({
                    "replaced": true,
                    "plugin": name,
                    "fiber_id": fiber_id,
                })),
            ))
        }
        // Refusal is BY DESIGN: nothing mutated, old provider still serving.
        Err(::cordis::CordisError::Configuration(reason)) => Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "replaced": false,
                "service": name,
                "reason": reason,
            })),
        )),
        Err(err) => Err(HttpError::from(ares_types::types::AppError::Internal(
            format!("provider replacement failed for {name}: {err}"),
        ))),
    }
}

/// GET /admin/cordis/undo — disposal-tree introspection (honest minimal
/// scope): per tracked fiber id, the labeled undo closures still pending on
/// its accumulator. Our undos are anonymous `Box<dyn FnOnce>` values; only
/// their [`cordis::UndoMeta`] labels + registration timestamps are
/// introspectable. 503 when no [`RegistryService`] is provided.
pub async fn list_cordis_undo_labels(
    State(ctx): State<Arc<Context>>,
) -> crate::Result<(StatusCode, Json<serde_json::Value>)> {
    let Some(registry) = ctx.get::<RegistryService>() else {
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "RegistryService is not provided on this context"
            })),
        ));
    };
    let mut fibers = Vec::new();
    for fid in registry.tracked_ids() {
        let Some(fiber) = registry.get_fiber(fid) else {
            continue;
        };
        fibers.push(serde_json::json!({
            "fiber_id": fid,
            "disposed": fiber.is_disposed(),
            "pending_undo_labels": fiber.pending_undo_labels(),
        }));
    }
    Ok((StatusCode::OK, Json(serde_json::json!({ "fibers": fibers }))))
}

pub fn routes() -> axum::Router<Arc<Context>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/cordis/services/{name}/retire",
            post(retire_cordis_service),
        )
        .route(
            "/cordis/services/{name}/provide",
            post(provide_cordis_service),
        )
        .route(
            "/cordis/services/{name}/replace",
            post(replace_cordis_service),
        )
        .route("/cordis/undo", get(list_cordis_undo_labels))
}

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ::cordis::Service;

/// Shared apply flow behind the reload endpoint and the entries mutations.
///
/// Thin adapter over [`cordis::reload_entries_from_disk`] — parse + compose +
/// diff-apply + classify, serialized against watcher batches by the shared
/// process-wide reload lock. The classified outcome maps back onto the exact
/// `(status, body)` tuples the legacy inline flow produced (`"reloaded":
/// false` markers included) so every caller answers uniformly; `Applied`
/// actions pass through untouched (including per-action failures, which the
/// response surfaces as `"ok": false` rows rather than a transport error).
async fn apply_entries_from_disk(
    ctx: &Arc<Context>,
) -> Result<Vec<cordis::loader::AppliedAction>, (StatusCode, serde_json::Value)> {
    match cordis::reload_entries_from_disk(ctx, &{
        // Resolve the program path from CurrentEntries first so a missing
        // service answers 503 with the legacy marker instead of a generic
        // failure at an arbitrary path.
        match ctx.get::<cordis::CurrentEntries>() {
            Some(ce) => ce.path.clone(),
            None => {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    serde_json::json!({
                        "reloaded": false,
                        "error": "CurrentEntries is not provided on this context",
                    }),
                ))
            }
        }
    })
    .await
    {
        cordis::ReloadOutcome::Applied { actions } => Ok(actions),
        cordis::ReloadOutcome::NoChange => Ok(Vec::new()),
        cordis::ReloadOutcome::Failed { error } => {
            // Distinguish "loader state absent" (503) from "file bad" (422).
            if error.contains("not provided on this context") {
                Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    serde_json::json!({ "reloaded": false, "error": error }),
                ))
            } else {
                Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    serde_json::json!({ "reloaded": false, "error": error }),
                ))
            }
        }
    }
}

/// Serialize per-action outcomes to the shared `"applied"` response shape.
fn applied_json(actions: &[cordis::loader::AppliedAction]) -> Vec<serde_json::Value> {
    actions
        .iter()
        .map(|a| match &a.status {
            Ok(()) => serde_json::json!({
                "id": a.id, "action": a.action, "ok": true, "verified": a.verified,
            }),
            Err(err) => serde_json::json!({
                "id": a.id, "action": a.action, "ok": false, "error": err,
                "verified": a.verified,
            }),
        })
        .collect()
}

/// Normalize an entry config so TOML serialization cannot fail: serde's
/// `#[serde(default)]` yields `Value::Null` for entries without an explicit
/// `[entry.config]` block, which `toml::to_string_pretty` rejects.
fn normalize_entry_config(mut entry: cordis::loader::Entry) -> cordis::loader::Entry {
    if entry.config.is_null() {
        entry.config = serde_json::json!({});
    }
    entry
}

/// Load the entries file as the desired tree for a mutation. A missing file
/// starts from an empty tree; an existing but unparsable file is a hard 422.
fn load_desired_tree(
    path: &std::path::Path,
) -> Result<cordis::loader::EntryTree, (StatusCode, serde_json::Value)> {
    if !path.exists() {
        return Ok(cordis::loader::EntryTree(vec![]));
    }
    cordis::loader::Loader::load_from_file(path).map_err(|e| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            serde_json::json!({ "applied": [], "error": e.to_string() }),
        )
    })
}

/// 503 guards shared by every entries endpoint that needs loader state.
fn require_loader_state(
    ctx: &Arc<Context>,
) -> Result<
    (Arc<cordis::LoaderJournal>, Arc<cordis::CurrentEntries>),
    (StatusCode, serde_json::Value),
> {
    let Some(journal) = ctx.get::<cordis::LoaderJournal>() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({
                "listed": false,
                "error": "LoaderJournal is not provided on this context",
            }),
        ));
    };
    let Some(current_entries) = ctx.get::<cordis::CurrentEntries>() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({
                "listed": false,
                "error": "CurrentEntries is not provided on this context",
            }),
        ));
    };
    Ok((journal, current_entries))
}

/// POST /admin/cordis/entries/reload — reload `cordis-entries.toml` through
/// the shared apply flow (`Loader::reload_current`, diffing against the
/// boot-applied tree). Returns per-action outcomes; 503 when loader state is
/// missing (library deployments without a file program), 422 when the file
/// cannot be read or parsed.
pub async fn reload_cordis_entries(
    State(ctx): State<Arc<Context>>,
) -> crate::Result<(StatusCode, Json<serde_json::Value>)> {
    match apply_entries_from_disk(&ctx).await {
        Ok(actions) => Ok((
            StatusCode::OK,
            Json(serde_json::json!({ "applied": applied_json(&actions) })),
        )),
        Err((status, body)) => Ok((status, Json(body))),
    }
}

/// GET /admin/cordis/entries — list the currently-applied tree plus the
/// pending diff against the on-disk file (without applying it).
pub async fn list_cordis_entries(
    State(ctx): State<Arc<Context>>,
) -> crate::Result<(StatusCode, Json<serde_json::Value>)> {
    use cordis::loader::{EntryConfigFillerHandle, Loader};
    use cordis::watcher::WATCH_DEBOUNCE;

    let (_journal, current_entries) = match require_loader_state(&ctx) {
        Ok(state) => state,
        Err((status, body)) => return Ok((status, Json(body))),
    };

    // Settle barrier: a PUT/DELETE/toggle that just rewrote the file may have
    // an in-flight watcher batch applying the same bytes. Await the watcher's
    // next settled outcome (bounded at 2x the debounce window) before reading
    // `CurrentEntries`, so this GET reports applied state instead of racing
    // the batch. Quiet systems simply time out here and read immediately.
    if let Some(barrier) = ctx.get::<cordis::SettleBarrier>() {
        let mut rx = (**barrier).clone();
        let _ = tokio::time::timeout(WATCH_DEBOUNCE + WATCH_DEBOUNCE, rx.changed()).await;
    }

    let current = current_entries.tree.lock().expect("entries lock").clone();
    let mut desired = match Loader::load_from_file(&current_entries.path) {
        Ok(tree) => tree,
        Err(e) => {
            return Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "listed": false,
                    "error": format!(
                        "failed to read or parse {}: {}",
                        current_entries.path.display(),
                        e
                    ),
                })),
            ));
        }
    };
    // Filler-hook parity with Loader::reload_current.
    if let Some(handle) = ctx.get::<EntryConfigFillerHandle>() {
        handle.0.fill_empty_entry_configs(&mut desired);
    }

    let loader = Loader;
    let pending: Vec<serde_json::Value> = loader
        .reconcile(&current, &desired)
        .iter()
        .map(|action| match action {
            cordis::loader::LoaderAction::RebuildFiber { id, .. } => {
                serde_json::json!({ "id": id, "action": "RebuildFiber" })
            }
            cordis::loader::LoaderAction::UpdateConfig { id, .. } => {
                serde_json::json!({ "id": id, "action": "UpdateConfig" })
            }
            cordis::loader::LoaderAction::Retire { id } => {
                serde_json::json!({ "id": id, "action": "Retire" })
            }
            cordis::loader::LoaderAction::Begin { id } => {
                serde_json::json!({ "id": id, "action": "Begin" })
            }
        })
        .collect();

    let current_json: Vec<serde_json::Value> = current
        .0
        .iter()
        .map(|e| serde_json::to_value(e).unwrap_or(serde_json::Value::Null))
        .collect();

    // Post-apply cycle report (additive response key): every fiber resolved to
    // its owning entry id; empty for a healthy graph.
    let dependency_cycles: Vec<Vec<String>> = cordis::loader::Loader::detect_cycle_entry_ids(&ctx);

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "current": current_json,
            "pending": pending,
            "path": current_entries.path.display().to_string(),
            "dependency_cycles": dependency_cycles,
        })),
    ))
}

/// PUT /admin/cordis/entries — upsert one entry (replace-by-id or append),
/// persist to the TOML program file, and apply through the same flow as
/// reload. Blank `id` / `plugin` are rejected with 400 InvalidInput.
pub async fn put_cordis_entry(
    State(ctx): State<Arc<Context>>,
    axum::Json(entry): axum::Json<cordis::loader::Entry>,
) -> crate::Result<(StatusCode, Json<serde_json::Value>)> {
    if entry.id.trim().is_empty() || entry.plugin.trim().is_empty() {
        return Err(HttpError::from(ares_types::types::AppError::InvalidInput(
            "entry id and plugin must be non-empty".to_string(),
        )));
    }

    let Some(current_entries) = ctx.get::<cordis::CurrentEntries>() else {
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "applied": [],
                "error": "CurrentEntries is not provided on this context",
            })),
        ));
    };
    let path = current_entries.path.clone();

    let mut tree = match load_desired_tree(&path) {
        Ok(tree) => tree,
        Err((status, body)) => return Ok((status, Json(body))),
    };
    let entry = normalize_entry_config(entry);
    match tree.0.iter_mut().find(|e| e.id == entry.id) {
        Some(slot) => *slot = entry,
        None => tree.0.push(entry),
    }
    if let Err(e) = tree.save_to_toml_file(&path) {
        return Ok((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "applied": [], "error": e.to_string() })),
        ));
    }

    match apply_entries_from_disk(&ctx).await {
        Ok(actions) => Ok((
            StatusCode::OK,
            Json(serde_json::json!({ "applied": applied_json(&actions) })),
        )),
        Err((status, body)) => Ok((status, Json(body))),
    }
}

/// DELETE /admin/cordis/entries/{id} — remove the entry with `id` from the
/// program file and apply the resulting retire. Unknown ids answer 404.
pub async fn delete_cordis_entry(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
) -> crate::Result<(StatusCode, Json<serde_json::Value>)> {
    if let Err((status, body)) = require_loader_state(&ctx) {
        return Ok((status, Json(body)));
    }
    let Some(current_entries) = ctx.get::<cordis::CurrentEntries>() else {
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "deleted": false,
                "error": "CurrentEntries is not provided on this context",
            })),
        ));
    };
    let path = current_entries.path.clone();

    let mut tree = match load_desired_tree(&path) {
        Ok(tree) => tree,
        Err((status, body)) => return Ok((status, Json(body))),
    };
    let before = tree.0.len();
    tree.0.retain(|e| e.id != id);
    if tree.0.len() == before {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "deleted": false,
                "error": "no such entry",
            })),
        ));
    }
    tree.0 = tree.0.drain(..).map(normalize_entry_config).collect();
    if let Err(e) = tree.save_to_toml_file(&path) {
        return Ok((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "applied": [], "error": e.to_string() })),
        ));
    }

    match apply_entries_from_disk(&ctx).await {
        Ok(actions) => Ok((
            StatusCode::OK,
            Json(serde_json::json!({ "applied": applied_json(&actions) })),
        )),
        Err((status, body)) => Ok((status, Json(body))),
    }
}

/// POST /admin/cordis/entries/{id}/toggle — flip `disabled` on the matching
/// entry, persist, and apply (disabled → Retire, re-enabled → Begin).
pub async fn toggle_cordis_entry(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
) -> crate::Result<(StatusCode, Json<serde_json::Value>)> {
    if let Err((status, body)) = require_loader_state(&ctx) {
        return Ok((status, Json(body)));
    }
    let Some(current_entries) = ctx.get::<cordis::CurrentEntries>() else {
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "toggled": false,
                "error": "CurrentEntries is not provided on this context",
            })),
        ));
    };
    let path = current_entries.path.clone();

    let mut tree = match load_desired_tree(&path) {
        Ok(tree) => tree,
        Err((status, body)) => return Ok((status, Json(body))),
    };
    let Some(entry) = tree.0.iter_mut().find(|e| e.id == id) else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "toggled": false,
                "error": "no such entry",
            })),
        ));
    };
    entry.disabled = !entry.disabled;
    let disabled = entry.disabled;
    tree.0 = tree.0.drain(..).map(normalize_entry_config).collect();
    if let Err(e) = tree.save_to_toml_file(&path) {
        return Ok((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "applied": [], "error": e.to_string() })),
        ));
    }

    match apply_entries_from_disk(&ctx).await {
        Ok(actions) => Ok((
            StatusCode::OK,
            Json(serde_json::json!({
                "applied": applied_json(&actions),
                "disabled": disabled,
            })),
        )),
        Err((status, body)) => Ok((status, Json(body))),
    }
}

/// GET /admin/cordis/events — per-event dispatch counters from the
/// `EventsService`. Returns 503 when the service is not provided.
pub async fn cordis_event_metrics(
    State(ctx): State<Arc<Context>>,
) -> crate::Result<(StatusCode, Json<serde_json::Value>)> {
    let Some(events) = ctx.get::<EventsService>() else {
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "total_dispatched": 0,
                "by_event": {},
                "error": "EventsService is not provided on this context",
            })),
        ));
    };
    let (total, by_event) = events.dispatch_snapshot();
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "total_dispatched": total,
            "by_event": by_event.into_iter().collect::<HashMap<String, u64>>(),
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicU64;

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
        assert!(
            provide_cordis_service(State(ctx.clone()), Path("nope".into()))
                .await
                .is_err()
        );
    }

    /// Registry-backed plugin providing `BarService`; used to build a real
    /// Active consumer fiber that declares an inject on `EventsService`.
    struct EventsDependentPlugin;
    impl ::cordis::Plugin for EventsDependentPlugin {
        type Config = ();
        type Provides = BarService;
        fn apply(
            &self,
            _ctx: &Arc<Context>,
            _cfg: Self::Config,
        ) -> Result<Arc<BarService>, ::cordis::CordisError> {
            Ok(Arc::new(BarService(5)))
        }
    }

    #[derive(Debug)]
    struct BarService(u64);
    impl ::cordis::Service for BarService {}

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retire_guarded_by_active_consumer_then_allowed_after_drop() {
        let ctx = Context::new_root();
        ctx.provide(ReflectService::new());
        ctx.provide(RegistryService::new());
        ctx.provide(EventsService::new());

        // Register the dependent THROUGH the registry so it is Active with a
        // recorded registration realm, then declare its inject on Events.
        let registry = ctx.get::<RegistryService>().unwrap();
        let dep_fid = registry
            .register(&ctx, EventsDependentPlugin, ())
            .expect("dependent registration");
        registry
            .get_fiber(dep_fid)
            .unwrap()
            .declare_inject::<EventsService>();
        assert!(matches!(
            registry.get_fiber(dep_fid).unwrap().state(),
            ::cordis::FiberState::Active { .. }
        ));

        // Retire must be REFUSED: one active consumer still relies on it.
        let resp = retire_cordis_service(State(ctx.clone()), Path("events_service".into()))
            .await
            .expect("handler");
        assert_eq!(resp.0, StatusCode::CONFLICT);
        assert_eq!(resp.1 .0["retired"], json!(false));
        assert_eq!(resp.1 .0["reason"], json!("guarded"));
        assert_eq!(resp.1 .0["consumers"], json!(1));
        assert!(
            ctx.get::<EventsService>().is_some(),
            "service stays provided under guard"
        );

        // Drop the consumer: its effects unwind and reliance drops to zero.
        let _ = registry.get_fiber(dep_fid).unwrap().dispose().await;
        assert!(ctx.get::<BarService>().is_none());

        // Retire now succeeds.
        let resp = retire_cordis_service(State(ctx.clone()), Path("events_service".into()))
            .await
            .expect("handler");
        assert_eq!(resp.0, StatusCode::OK);
        assert_eq!(resp.1 .0["retired"], json!(true));
        assert!(ctx.get::<EventsService>().is_none());
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
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );

        let dir = std::env::temp_dir().join(format!("cordis-reload-test-{}", std::process::id()));
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
        let (status, Json(body)) = reload_cordis_entries(State(ctx.clone()))
            .await
            .expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["applied"][0]["action"], "begin");
        assert_eq!(body["applied"][0]["ok"], true);
        assert!(ctx.get::<Probe>().is_some(), "calculator instantiated");

        // 2) remove it → Retire-ok
        std::fs::write(&path, "").expect("write entries v3");
        let (status, Json(body)) = reload_cordis_entries(State(ctx.clone()))
            .await
            .expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["applied"][0]["action"], "retire");
        assert_eq!(body["applied"][0]["ok"], true);
        assert!(ctx.get::<Probe>().is_none(), "retired service removed");

        // current tree tracks desired across both applies
        {
            let tree = current_entries.tree.lock().unwrap();
            assert_eq!(tree.0.len(), 0);
            assert_eq!(
                Loader
                    .reconcile(
                        &tree,
                        &EntryTree(vec![Entry {
                            id: "calc".into(),
                            plugin: "CalculatorService".into(),
                            config: serde_json::Value::Null,
                            disabled: false,
                            isolate: None,
                            intercept: Default::default(),
                        }])
                    )
                    .len(),
                1,
                "sanity: diff detects a re-add"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    // --- entries management (list / put / delete / toggle) ---

    /// Minimal plugin factory shared by the entries fixtures. The instance
    /// value is an atomic so replace tests can observe WHICH configuration
    /// is serving (old vs new) through the same handle.
    #[derive(Debug)]
    struct Probe(std::sync::atomic::AtomicU64);
    impl Service for Probe {}

    fn probe_entry(id: &str, disabled: bool) -> cordis::loader::Entry {
        cordis::loader::Entry {
            id: id.to_string(),
            plugin: "CalculatorService".to_string(),
            config: serde_json::json!({}),
            disabled,
            isolate: None,
            intercept: Default::default(),
        }
    }

    const CALC_TOML_BLOCK: &str = "[[entry]]\nid = \"calc\"\nplugin = \"CalculatorService\"\ndisabled = false\n\n[entry.config]\n";

    /// Full working fixture: loader context (journal + registry + factory)
    /// plus a temp TOML file seeded with `initial_toml`; `CurrentEntries`
    /// starts with `current` as the applied tree. Mirrors the reload test
    /// fixture above.
    fn build_entries_fixture(
        tag: &str,
        initial_toml: &str,
        current: Vec<cordis::loader::Entry>,
    ) -> (Arc<Context>, std::path::PathBuf) {
        let ctx = Context::new_root();
        ctx.provide(ReflectService::new());
        cordis::LoaderJournal::provide_new(&ctx);
        ctx.provide(cordis::RegistryService::new());
        let registry = ctx.provide(cordis::PluginRegistry::new());

        registry.register(
            "CalculatorService",
            Arc::new(|ctx, cfg| {
                // Config-driven instance value so replace tests can prove the
                // new configuration took effect on the swapped provider.
                let v = cfg.get("v").and_then(|x| x.as_u64()).unwrap_or(1);
                let fut = ctx.plugin(Probe(AtomicU64::new(v)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );

        let dir =
            std::env::temp_dir().join(format!("cordis-entries-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmpdir");
        let path = dir.join("cordis-entries.toml");
        std::fs::write(&path, initial_toml).expect("seed entries file");

        let seed = cordis::CurrentEntries {
            tree: Arc::new(std::sync::Mutex::new(cordis::loader::EntryTree(current))),
            path: path.clone(),
        };
        ctx.provide_arc(Arc::new(seed));
        (ctx, dir)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn list_cordis_entries_reports_current_and_pending_diff() {
        use cordis::loader::EntryTree;

        let toml_body = format!("{CALC_TOML_BLOCK}\n[[entry]]\nid = \"calc2\"\nplugin = \"CalculatorService\"\ndisabled = false\n\n[entry.config]\n");
        let (ctx, dir) =
            build_entries_fixture("list-diff", &toml_body, vec![probe_entry("calc", false)]);
        let path = dir.join("cordis-entries.toml");

        let (status, Json(body)) = list_cordis_entries(State(ctx.clone())).await.expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["current"].as_array().unwrap().len(), 1);
        assert_eq!(body["current"][0]["id"], "calc");
        assert_eq!(body["path"], path.display().to_string());

        let pending = body["pending"].as_array().unwrap();
        assert!(
            pending
                .iter()
                .any(|p| p["id"] == "calc2" && p["action"] == "Begin"),
            "pending diff should contain Begin for calc2, got {pending:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The listing always carries `dependency_cycles`; a healthy (acyclic)
    /// graph — including library deployments without ledger state — reports
    /// an empty array.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn list_cordis_entries_reports_dependency_cycles_key() {
        use cordis::loader::EntryTree;

        let (ctx, dir) = build_entries_fixture(
            "list-cycles",
            CALC_TOML_BLOCK,
            vec![probe_entry("calc", false)],
        );

        let (status, Json(body)) = list_cordis_entries(State(ctx.clone())).await.expect("resp");
        assert_eq!(status, StatusCode::OK);
        let cycles = body["dependency_cycles"]
            .as_array()
            .expect("dependency_cycles key present");
        assert!(
            cycles.is_empty(),
            "acyclic fixture must report an empty array, got {cycles:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn put_cordis_entry_appends_and_applies_and_keeps_header() {
        use cordis::loader::EntryTree;

        let header_toml = format!(
            "# Cordis plugin entries loaded at startup.\n# Order matters.\n\n{CALC_TOML_BLOCK}"
        );
        let (ctx, dir) =
            build_entries_fixture("put-append", &header_toml, vec![probe_entry("calc", false)]);
        let path = dir.join("cordis-entries.toml");

        let mut new_entry = probe_entry("calc2", false);
        new_entry.config = serde_json::Value::Null; // handler must normalize
        let (status, Json(body)) = put_cordis_entry(State(ctx.clone()), axum::Json(new_entry))
            .await
            .expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["applied"][0]["id"], "calc2");
        assert_eq!(body["applied"][0]["action"], "begin");
        assert_eq!(body["applied"][0]["ok"], true);
        assert!(ctx.get::<Probe>().is_some(), "calculator instantiated");

        // Raw file keeps the comment header AND gains the new entry.
        let raw = std::fs::read_to_string(&path).expect("read back");
        assert!(
            raw.starts_with("# Cordis plugin entries loaded at startup."),
            "header preserved, got: {raw}"
        );
        assert!(raw.contains("# Order matters."));
        assert!(raw.contains("id = \"calc2\""));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delete_cordis_entry_removes_and_applies() {
        use cordis::loader::EntryTree;

        let (ctx, dir) =
            build_entries_fixture("delete", CALC_TOML_BLOCK, vec![probe_entry("calc", false)]);

        let (status, Json(body)) = delete_cordis_entry(State(ctx.clone()), Path("calc".into()))
            .await
            .expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["applied"][0]["id"], "calc");
        assert_eq!(body["applied"][0]["action"], "retire");
        assert_eq!(body["applied"][0]["ok"], true);
        assert!(ctx.get::<Probe>().is_none(), "retired service removed");

        // Unknown id answers 404 with deleted:false.
        let (status, Json(body)) = delete_cordis_entry(State(ctx.clone()), Path("nope".into()))
            .await
            .expect("resp");
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["deleted"], false);
        assert_eq!(body["error"], "no such entry");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn toggle_cordis_entry_flips_disabled_state() {
        use cordis::loader::EntryTree;

        let (ctx, _dir) =
            build_entries_fixture("toggle", CALC_TOML_BLOCK, vec![probe_entry("calc", false)]);

        // First toggle: disabled=false → true → Retire.
        let (status, Json(body)) = toggle_cordis_entry(State(ctx.clone()), Path("calc".into()))
            .await
            .expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["disabled"], true);
        let retire_ok = body["applied"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["action"] == "retire" && a["ok"] == true);
        assert!(
            retire_ok,
            "expected retire-ok action, got {:?}",
            body["applied"]
        );
        assert!(ctx.get::<Probe>().is_none());

        // Second toggle: disabled=true → false → Begin again.
        let (status, Json(body)) = toggle_cordis_entry(State(ctx.clone()), Path("calc".into()))
            .await
            .expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["disabled"], false);
        let begin_ok = body["applied"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["action"] == "begin" && a["ok"] == true);
        assert!(
            begin_ok,
            "expected begin-ok action, got {:?}",
            body["applied"]
        );
        assert!(ctx.get::<Probe>().is_some());

        std::fs::remove_dir_all(_dir).ok();
    }

    /// The swap keeps the SAME factory label (`CalculatorService`); the new
    /// config only changes the instance value the factory reads, so a 200
    /// proves the config took effect. Continuity is asserted via
    /// `ctx.get::<Probe>()` before AND after: the bridge/promote swap never
    /// lets the key become unprovided.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replace_endpoint_replaces_known_plugin() {
        use std::sync::atomic::{AtomicU64, Ordering};

        // Empty CURRENT tree: boot-apply must diff to a Begin that
        // instantiates the provider and journals its live fiber.
        let (ctx, dir) = build_entries_fixture("replace-ok", CALC_TOML_BLOCK, vec![]);

        {
            let current = ctx.get::<cordis::CurrentEntries>().unwrap();
            let mut tree = current.tree.lock().expect("entries lock").clone();
            assert!(
                tree.0.is_empty(),
                "fixture must start unapplied for the Begin diff"
            );
            cordis::loader::Loader::apply(
                &ctx,
                &mut tree,
                &cordis::loader::EntryTree(vec![probe_entry("calc", false)]),
                &ctx.get::<cordis::LoaderJournal>().unwrap(),
            )
            .await;
            *current.tree.lock().expect("entries lock") = tree;
        }
        let old_fid = ctx
            .get::<cordis::LoaderJournal>()
            .unwrap()
            .get("calc")
            .expect("journaled")
            .fiber_id
            .expect("tracked");
        assert_eq!(ctx.get::<Probe>().unwrap().0.load(Ordering::SeqCst), 1);
        assert!(matches!(
            ctx.get::<cordis::RegistryService>()
                .unwrap()
                .get_fiber(old_fid)
                .unwrap()
                .state(),
            ::cordis::FiberState::Active { .. }
        ));

        // Replace with a new config for the same plugin label.
        let (status, Json(body)) = replace_cordis_service(
            State(ctx.clone()),
            Path("CalculatorService".into()),
            Json(json!({"config": {"v": 9}})),
        )
        .await
        .expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["replaced"], json!(true));
        assert_eq!(body["plugin"], "CalculatorService");
        let new_fid = body["fiber_id"].as_u64().expect("new fiber id");

        // New instance live with the NEW config; continuity held throughout.
        assert!(ctx.get::<Probe>().is_some(), "service still resolves");
        assert_eq!(
            ctx.get::<Probe>().unwrap().0.load(Ordering::SeqCst),
            9,
            "instance reflects the new configuration"
        );
        assert_ne!(new_fid, old_fid, "fresh registration fiber");
        assert!(matches!(
            ctx.get::<cordis::RegistryService>()
                .unwrap()
                .get_fiber(new_fid)
                .unwrap()
                .state(),
            ::cordis::FiberState::Active { .. }
        ));
        let rec = ctx
            .get::<cordis::LoaderJournal>()
            .unwrap()
            .get("calc")
            .unwrap();
        assert_eq!(rec.fiber_id, Some(new_fid), "journal advanced to new fiber");
        assert_eq!(rec.config, json!({"v": 9}));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Unknown plugin label → 409 refusal; nothing was replaced.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replace_endpoint_unknown_plugin_409() {
        let (ctx, dir) = build_entries_fixture("replace-unknown", "", vec![]);

        let (status, Json(body)) = replace_cordis_service(
            State(ctx.clone()),
            Path("NoSuchFactory".into()),
            Json(json!({"config": {}})),
        )
        .await
        .expect("resp");
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["replaced"], json!(false));
        let reason = body["reason"].as_str().expect("reason string");
        assert!(
            reason.contains("no journaled entry"),
            "refusal names the unknown label, got {reason}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Missing / malformed body → 400 naming the explicit `{"config": …}`
    /// requirement. Bare config objects at top level are deliberately NOT
    /// accepted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replace_endpoint_missing_config_400() {
        let (ctx, dir) = build_entries_fixture("replace-missing", "", vec![]);

        for bad in [json!({}), json!("nope"), json!([1, 2])] {
            let (status, Json(body)) = replace_cordis_service(
                State(ctx.clone()),
                Path("CalculatorService".into()),
                Json(bad),
            )
            .await
            .expect("handler");
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["replaced"], json!(false));
            body["error"]
                .as_str()
                .expect("error names the config requirement");
        }
        let (status, Json(body)) = replace_cordis_service(
            State(ctx.clone()),
            Path("CalculatorService".into()),
            Json(json!({"config": {"v": 3}})),
        )
        .await
        .expect("well-formed body passes the guard");
        // No journaled provider in this bare fixture → 409 refusal, but that
        // proves the 400 guard sits in front of execution.
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["replaced"], false);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Absent loader state → 503 mirroring the sibling endpoints' shape.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replace_endpoint_missing_journal_503() {
        let ctx = Context::new_root();

        let (status, Json(body)) = replace_cordis_service(
            State(ctx),
            Path("CalculatorService".into()),
            Json(json!({"config": {}})),
        )
        .await
        .expect("handler");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["replaced"], json!(false));
        assert_eq!(
            body["error"],
            json!("LoaderJournal is not provided on this context")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn put_cordis_entry_rejects_blank_plugin() {
        use cordis::loader::EntryTree;

        let (ctx, dir) = build_entries_fixture("put-blank", "", vec![]);

        let mut bad = probe_entry("x", false);
        bad.plugin = "  ".to_string();
        let err = put_cordis_entry(State(ctx.clone()), axum::Json(bad))
            .await
            .expect_err("blank plugin must be rejected");
        assert_eq!(err.0.status_code(), 400);

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod event_metrics_tests {
    use super::*;
    use ::cordis::Dispatch;
    use serde_json::json;

    #[tokio::test]
    async fn cordis_event_metrics_counts_dispatches() {
        let ctx = Context::new_root();
        ctx.provide(EventsService::new());
        let events = ctx.get::<EventsService>().expect("events provided");

        // Two Emit dispatches on agent.usage + one Bail on scheduler.admit.
        for _ in 0..2 {
            events
                .dispatch(
                    cordis::events_catalog::ev::AGENT_USAGE.into(),
                    json!({"tokens": 1}),
                    Dispatch::Emit,
                )
                .await
                .unwrap();
        }
        events
            .dispatch(
                cordis::events_catalog::ev::SCHEDULER_ADMIT.into(),
                json!({"agent_name": "a"}),
                Dispatch::Bail,
            )
            .await
            .unwrap();

        let resp = cordis_event_metrics(State(ctx.clone()))
            .await
            .expect("handler");
        assert_eq!(resp.0, StatusCode::OK);
        assert_eq!(resp.1 .0["total_dispatched"], json!(3));
        assert_eq!(resp.1 .0["by_event"]["agent.usage"], json!(2));
        assert_eq!(resp.1 .0["by_event"]["scheduler.admit"], json!(1));
    }

    #[tokio::test]
    async fn cordis_event_metrics_missing_service_is_503() {
        let ctx = Context::new_root();
        let resp = cordis_event_metrics(State(ctx)).await.expect("handler");
        assert_eq!(resp.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            resp.1 .0["error"],
            json!("EventsService is not provided on this context")
        );
    }

    /// Undo-label introspection: labeled undos surface with their labels in
    /// registration order; the missing-registry path answers 503.
    #[tokio::test]
    async fn cordis_undo_labels_lists_pending_and_flags_disposed() {
        #[derive(Debug)]
        struct UndoProbe(u64);
        impl ::cordis::Service for UndoProbe {}
        struct UndoProbePlugin;
        impl ::cordis::Plugin for UndoProbePlugin {
            type Config = ();
            type Provides = UndoProbe;
            fn apply(
                &self,
                _ctx: &Arc<Context>,
                _cfg: Self::Config,
            ) -> Result<Arc<UndoProbe>, ::cordis::CordisError> {
                Ok(Arc::new(UndoProbe(1)))
            }
        }

        let ctx = Context::new_root();
        let registry = ctx.provide(RegistryService::new());
        let fid = registry
            .register(&ctx, UndoProbePlugin, ())
            .expect("registration");
        let fiber = registry.get_fiber(fid).unwrap();
        fiber.push_undo_labeled(
            cordis::UndoMeta::new("provide:probe"),
            Box::new(|| {}),
        );

        let resp = list_cordis_undo_labels(State(ctx.clone()))
            .await
            .expect("handler");
        assert_eq!(resp.0, StatusCode::OK);
        let fibers = resp.1 .0["fibers"].as_array().unwrap();
        let entry = fibers
            .iter()
            .find(|f| f["fiber_id"] == json!(fid))
            .expect("registered fiber listed");
        // The registration's own provide undo carries the default "unnamed"
        // label; our explicit labeled push lands after it.
        let labels = entry["pending_undo_labels"].as_array().unwrap();
        assert_eq!(labels.last().unwrap(), &json!("provide:probe"));
        assert_eq!(labels[0], json!("unnamed"));
        assert_eq!(entry["disposed"], json!(false));

        // After dispose the label list drains; the (now pruned) fiber no
        // longer appears.
        let _ = fiber.dispose().await;
        let _ = registry.prune_disposed();
        let resp = list_cordis_undo_labels(State(ctx.clone()))
            .await
            .expect("handler");
        let fibers = resp.1 .0["fibers"].as_array().unwrap();
        assert!(
            !fibers.iter().any(|f| f["fiber_id"] == json!(fid)),
            "disposed+pruned fiber must vanish from introspection"
        );

        // 503 when no RegistryService is on the context.
        let bare = Context::new_root();
        let resp = list_cordis_undo_labels(State(bare)).await.expect("h");
        assert_eq!(resp.0, StatusCode::SERVICE_UNAVAILABLE);
    }
}
