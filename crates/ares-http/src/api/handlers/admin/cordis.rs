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
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "fibers": fibers })),
    ))
}

/// GET /admin/cordis/services — one summary row per tracked fiber:
/// `{fiber_id, state, error, disposed, pending_undo_count}`. `state` is the
/// debug form of the fiber's [`cordis::FiberState`] (Active, Inactive,
/// Loading, Failed, Reloading, Unloading); `error` carries the resting
/// terminal-state message when present. 503 mirrors [`list_cordis_undo_labels`]
/// when no [`RegistryService`] is provided.
pub async fn list_cordis_services(
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
            "state": format!("{:?}", fiber.state()),
            "error": fiber.error(),
            "disposed": fiber.is_disposed(),
            "pending_undo_count": fiber.pending_undo_labels().len(),
        }));
    }
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "fibers": fibers })),
    ))
}

/// Process-global bounded ring for LLM call records, read by
/// GET /admin/cordis/logs. Boot wiring (installing a shared ring as an
/// exporter on the LLM layer's ExporterRouter) belongs to the telemetry
/// installer; this seam only guarantees the endpoint answers — empty when no
/// ring was installed at boot.
static LOG_RING: LazyLock<RwLock<Option<Arc<ares_llm::exporter::LogRing>>>> =
    LazyLock::new(|| RwLock::new(None));

/// Install the process-global [`LogRing`] (boot-time; idempotent). Returns
/// the ring actually in effect — the previously-installed one when this call
/// raced or followed an earlier install. Boot wiring (registering the ring
/// as a [`ares_llm::exporter::LogExporter`] on the LLM layer's router)
/// belongs to the telemetry installer; this seam only guarantees the logs
/// endpoint has something to read.
pub fn install_log_ring(
    ring: Arc<ares_llm::exporter::LogRing>,
) -> Arc<ares_llm::exporter::LogRing> {
    let mut slot = LOG_RING.write().expect("log ring lock");
    if let Some(existing) = slot.as_ref() {
        return existing.clone();
    }
    *slot = Some(ring.clone());
    ring
}

fn active_log_ring() -> Option<Arc<ares_llm::exporter::LogRing>> {
    LOG_RING.read().expect("log ring lock").clone()
}

/// GET /admin/cordis/logs — last-N LLM call records from the process-global
/// bounded [`LogRing`] (oldest first), serialized as JSON. Answers an empty
/// list when no ring was installed at boot.
pub async fn cordis_logs() -> crate::Result<(StatusCode, Json<serde_json::Value>)> {
    let records = match active_log_ring() {
        Some(ring) => ring.snapshot(),
        None => Vec::new(),
    };
    let logs: Vec<serde_json::Value> = records
        .iter()
        .map(|r| {
            serde_json::json!({
                "step_index": r.step_index,
                "provider": r.provider,
                "model": r.model,
                "prompt_tokens": r.prompt_tokens,
                "completion_tokens": r.completion_tokens,
                "latency_ms": r.latency_ms,
                "status": r.status,
                "cached_tokens": r.cached_tokens,
                "total_time_ms": r.total_time_ms,
            })
        })
        .collect();
    Ok((StatusCode::OK, Json(serde_json::json!({ "logs": logs }))))
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
        .route("/cordis/services", get(list_cordis_services))
        .route("/cordis/logs", get(cordis_logs))
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
        let mut rx = (*barrier).clone();
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

/// POST /admin/cordis/entries/{id}/move — relocate the entry (and its whole
/// `{id}:*` descendant namespace) under a new parent.
///
/// Body: `{"parent": "group-id" | null, "position": N}` — `parent: null`
/// moves to the tree root; an absent `position` appends after the target's
/// existing children. Execution goes through
/// [`cordis::loader::Loader::move_entry`]: invalid moves (unknown ids,
/// moving under one's own descendant, id collisions) answer 409 WITHOUT
/// touching the file or the live tree; a valid move re-keys the journal
/// records with fiber ids PRESERVED (pure structural moves never restart
/// fibers), persists the renamed tree, and applies the diff. Responds
/// `{moved: true, renamed, applied}`; unknown ids 404, missing loader state
/// 503.
pub async fn move_cordis_entry(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> crate::Result<(StatusCode, Json<serde_json::Value>)> {
    if let Err((status, body)) = require_loader_state(&ctx) {
        return Ok((status, Json(body)));
    }
    let Some(current_entries) = ctx.get::<cordis::CurrentEntries>() else {
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "moved": false,
                "error": "CurrentEntries is not provided on this context",
            })),
        ));
    };
    let path = current_entries.path.clone();

    let parent = body.get("parent").map(|p| {
        p.as_str().map(str::to_string).ok_or_else(|| {
            HttpError::from(ares_types::types::AppError::InvalidInput(
                "\"parent\" must be a string or null".to_string(),
            ))
        })
    });
    let parent = match parent {
        Some(Ok(p)) => Some(p),
        Some(Err(e)) => return Err(e),
        None => None,
    };
    let position = match body.get("position") {
        Some(v) => match v.as_u64() {
            Some(p) => p as usize,
            None => {
                return Err(HttpError::from(ares_types::types::AppError::InvalidInput(
                    "\"position\" must be a non-negative integer".to_string(),
                )));
            }
        },
        None => usize::MAX,
    };

    let mut tree = match load_desired_tree(&path) {
        Ok(tree) => tree,
        Err((status, body)) => return Ok((status, Json(body))),
    };
    if !tree.0.iter().any(|e| e.id == id) {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "moved": false, "error": "no such entry" })),
        ));
    }

    let journal = ctx.get::<cordis::LoaderJournal>().expect("loader state");
    let outcome = cordis::loader::Loader::move_entry(
        &ctx,
        &mut tree,
        &journal,
        &id,
        parent.as_deref(),
        position,
    )
    .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(e) => {
            return Ok((
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "moved": false, "error": e.to_string() })),
            ));
        }
    };

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
                "moved": true,
                "noop": outcome.noop,
                "renamed": outcome.renamed,
                "applied": applied_json(&actions),
            })),
        )),
        Err((status, body)) => Ok((status, Json(body))),
    }
}

/// PATCH /admin/cordis/entries/{id} — partial update of one entry: only the
/// fields present in the [`cordis::loader::EntryUpdate`] body are applied
/// (`config`, `disabled`, `isolate`, `intercept`); everything else is left
/// untouched, so an empty body is a validated no-op that still persists and
/// re-applies the unchanged tree.
///
/// Present `parent` / `position` body fields MOVE the entry first (through
/// [`cordis::loader::Loader::move_entry`], preserving live fiber identity on
/// pure structural moves), THEN the remaining field updates land in one call.
/// An invalid move answers 409 without touching the file or the live tree.
/// Persists to the TOML program file and applies through the same flow as
/// reload; responds with the post-patch entry (plus `renamed` old→new pairs
/// when a move ran). Unknown ids answer 404.
pub async fn patch_cordis_entry(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    axum::Json(update): axum::Json<cordis::loader::EntryUpdate>,
) -> crate::Result<(StatusCode, Json<serde_json::Value>)> {
    if let Err((status, body)) = require_loader_state(&ctx) {
        return Ok((status, Json(body)));
    }
    let Some(current_entries) = ctx.get::<cordis::CurrentEntries>() else {
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "patched": false,
                "error": "CurrentEntries is not provided on this context",
            })),
        ));
    };
    let path = current_entries.path.clone();

    let mut tree = match load_desired_tree(&path) {
        Ok(tree) => tree,
        Err((status, body)) => return Ok((status, Json(body))),
    };
    if !tree.0.iter().any(|e| e.id == id) {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "patched": false,
                "error": "no such entry",
            })),
        ));
    };

    // MOVE phase: a present `parent`/`position` field relocates the entry —
    // renaming the subtree namespace — BEFORE any field updates land.
    let mut renamed: Vec<(String, String)> = Vec::new();
    if update.parent.is_some() || update.position.is_some() {
        let current_parent = tree
            .0
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.position.as_ref())
            .and_then(|p| p.parent.clone());
        let target = match &update.parent {
            // Explicit null = move to the tree root.
            Some(explicit) => explicit.clone(),
            // Position-only request reorders within the CURRENT parent.
            None => current_parent,
        };
        // Absent position appends after the target's existing children.
        let position = update.position.unwrap_or(usize::MAX);
        let journal = ctx.get::<cordis::LoaderJournal>().expect("loader state");
        match cordis::loader::Loader::move_entry(
            &ctx,
            &mut tree,
            &journal,
            &id,
            target.as_deref(),
            position,
        )
        .await
        {
            Ok(outcome) => renamed = outcome.renamed,
            Err(e) => {
                return Ok((
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "patched": false,
                        "error": e.to_string(),
                    })),
                ));
            }
        }
    }
    let final_id = renamed
        .last()
        .map(|(_, new)| new.clone())
        .unwrap_or_else(|| id.clone());

    let Some(entry) = tree.0.iter_mut().find(|e| e.id == final_id) else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "patched": false,
                "error": "no such entry",
            })),
        ));
    };
    update.apply_to(entry);
    tree.0 = tree.0.drain(..).map(normalize_entry_config).collect();
    if let Err(e) = tree.save_to_toml_file(&path) {
        return Ok((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "applied": [], "error": e.to_string() })),
        ));
    }

    // Structured issues below describe THIS apply only: drop any record an
    // earlier patch left for this entry before running the pre-flights.
    let _ = cordis::error::take_trial_validation(&final_id);
    match apply_entries_from_disk(&ctx).await {
        Ok(actions) => {
            let patched = ctx
                .get::<cordis::CurrentEntries>()
                .and_then(|ce| {
                    ce.tree
                        .lock()
                        .ok()
                        .map(|t| t.0.iter().find(|e| e.id == final_id).cloned())
                })
                .flatten();
            let Some(patched) = patched else {
                return Ok((
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "patched": false,
                        "error": "entry vanished during apply",
                    })),
                ));
            };
            let mut body = serde_json::json!({
                "applied": applied_json(&actions),
                "patched": true,
                "entry": patched,
            });
            if !renamed.is_empty() {
                body["renamed"] = serde_json::to_value(&renamed).unwrap_or_default();
            }
            Ok((StatusCode::OK, Json(body)))
        }
        Err((status, mut body)) => {
            // When the failing step was a config pre-flight, the loader
            // trial stashed machine-readable issues for this entry; attach
            // them alongside the legacy `error` string.
            if let Some(validation) = cordis::error::take_trial_validation(&final_id) {
                if let Ok(issues) = serde_json::to_value(&validation.issues) {
                    body["issues"] = issues;
                }
            }
            Ok((status, Json(body)))
        }
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

        let _ = retire_cordis_service(State(ctx.clone()), Path("events_service".into()))
            .await
            .expect("handler");
        // remove() notified dependents; give the spawned refresh a beat.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(matches!(
            fiber.state(),
            ::cordis::FiberState::Inactive { .. }
        ));

        let _ = provide_cordis_service(State(ctx.clone()), Path("events_service".into()))
            .await
            .expect("handler");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(matches!(fiber.state(), ::cordis::FiberState::Active { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reload_endpoint_applies_add_and_retire_from_file() {
        use cordis::loader::{Entry, EntryTree, Loader};
        let _loader = Loader;

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
                            position: None,
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
            position: None,
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

    /// Patching only `config` leaves `disabled`, `isolate`, and `intercept`
    /// untouched; the response carries the post-patch entry and the apply
    /// reports the config change.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn patch_config_only_updates_single_field() {
        // Seed file AND current tree identically: the on-disk file is the
        // source of truth during apply, so the isolate must live in the file
        // to survive the post-patch re-apply.
        let initial_toml = "[[entry]]\nid = \"calc\"\nplugin = \"CalculatorService\"\ndisabled = false\nisolate = \"tenant-a\"\n\n[entry.config]\nv = 1\n";
        let mut seeded = probe_entry("calc", false);
        seeded.config = serde_json::json!({"v": 1});
        seeded.isolate = Some("tenant-a".into());
        let (ctx, dir) = build_entries_fixture("patch-config", initial_toml, vec![seeded]);

        let update = cordis::loader::EntryUpdate {
            config: Some(serde_json::json!({"v": 7})),
            disabled: None,
            isolate: None,
            intercept: None,
            parent: None,
            position: None,
        };
        let (status, Json(body)) =
            patch_cordis_entry(State(ctx.clone()), Path("calc".into()), axum::Json(update))
                .await
                .expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["patched"], json!(true));
        assert_eq!(
            body["entry"],
            json!({
                "id": "calc",
                "plugin": "CalculatorService",
                "config": {"v": 7},
                "disabled": false,
                "isolate": "tenant-a",
                "intercept": {},
            }),
            "only config changed; everything else untouched"
        );
        assert_eq!(body["entry"]["config"]["v"], 7);
        assert_eq!(body["entry"]["disabled"], false);
        assert_eq!(body["entry"]["isolate"], "tenant-a");

        // The on-disk program carries the patched config.
        let raw = std::fs::read_to_string(dir.join("cordis-entries.toml")).expect("read back");
        assert!(raw.contains("v = 7"), "config persisted, got: {raw}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An empty body is a validated no-op: 200 with the entry unchanged and
    /// still persisted + re-applied through the shared flow.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn patch_empty_body_is_noop() {
        let (ctx, dir) = build_entries_fixture(
            "patch-empty",
            CALC_TOML_BLOCK,
            vec![probe_entry("calc", false)],
        );

        let (status, Json(body)) = patch_cordis_entry(
            State(ctx.clone()),
            Path("calc".into()),
            axum::Json(cordis::loader::EntryUpdate::default()),
        )
        .await
        .expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["patched"], json!(true));
        assert_eq!(
            body["entry"],
            serde_json::to_value(probe_entry("calc", false)).unwrap()
        );

        // The same entry set round-trips: exactly one calc entry remains and
        // no field was altered by the empty patch body.
        let after = std::fs::read_to_string(dir.join("cordis-entries.toml")).expect("read back");
        let ids: Vec<&str> = after
            .lines()
            .filter_map(|l| l.strip_prefix("id = "))
            .collect();
        assert_eq!(
            ids,
            vec!["\"calc\""],
            "no-op patch keeps the entry set intact"
        );
        assert!(after.contains("id = \"calc\""));
        assert!(!after.contains("v ="), "config untouched by empty body");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Unknown id answers 404 with `patched:false`; nothing was written or
    /// applied.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn patch_unknown_id_404s() {
        let (ctx, dir) = build_entries_fixture(
            "patch-unknown",
            CALC_TOML_BLOCK,
            vec![probe_entry("calc", false)],
        );
        let before = std::fs::read_to_string(dir.join("cordis-entries.toml")).expect("read");

        let update = cordis::loader::EntryUpdate {
            config: Some(serde_json::json!({"v": 3})),
            ..Default::default()
        };
        let (status, Json(body)) =
            patch_cordis_entry(State(ctx.clone()), Path("nope".into()), axum::Json(update))
                .await
                .expect("resp");
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["patched"], json!(false));
        assert_eq!(body["error"], "no such entry");

        // Nothing persisted.
        assert_eq!(
            std::fs::read_to_string(dir.join("cordis-entries.toml")).expect("read back"),
            before,
            "404 must not touch the file"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A config pre-flight failing with structured validation issues answers
    /// the PATCH 4xx body with a machine-readable `issues` array alongside
    /// the legacy `error` string. A follow-up well-formed patch succeeds,
    /// proving the failed trial left no stale slot behind.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn patch_endpoint_returns_structured_issues_on_bad_config() {
        let (ctx, dir) = build_entries_fixture(
            "patch-validation",
            CALC_TOML_BLOCK,
            vec![probe_entry("calc", false)],
        );
        // The trial pre-flight only runs for journaled entries; seed the
        // record the boot apply would have left (fiber id intentionally
        // untracked so the failure can only come from the trial itself).
        let journal = ctx.get::<cordis::LoaderJournal>().expect("journal");
        journal.upsert("calc", "CalculatorService", serde_json::json!({}), Some(1));

        // Override the fixture factory: the patched config asks for a
        // validation-shaped rejection carrying a placed issue.
        let registry = ctx.get::<cordis::PluginRegistry>().expect("registry");
        registry.register(
            "CalculatorService",
            Arc::new(|ctx, cfg| {
                if cfg.get("reject").and_then(|x| x.as_bool()) == Some(true) {
                    return Err(cordis::CordisError::validation(vec![
                        cordis::ValidationIssue::new("missing url").at(["calc", "url"]),
                    ]));
                }
                let v = cfg.get("v").and_then(|x| x.as_u64()).unwrap_or(1);
                let fut = ctx.plugin(Probe(AtomicU64::new(v)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );

        let bad_update = cordis::loader::EntryUpdate {
            config: Some(serde_json::json!({"reject": true})),
            ..Default::default()
        };
        let (status, Json(body)) = patch_cordis_entry(
            State(ctx.clone()),
            Path("calc".into()),
            axum::Json(bad_update),
        )
        .await
        .expect("resp");

        // Legacy failure shape intact…
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["reloaded"], json!(false));
        assert!(
            body["error"]
                .as_str()
                .map(|e| e.contains("config pre-flight failed"))
                .unwrap_or(false),
            "legacy error names the pre-flight: {}",
            body["error"]
        );

        // …plus the structured issues array from the stashed trial result.
        assert_eq!(
            body["issues"],
            json!([{ "message": "missing url", "path": ["calc", "url"] }]),
            "structured issues accompany the error field: {body}"
        );

        // The failed trial must leave nothing stashed: a healthy patch now
        // goes through cleanly (200) instead of tripping stale issues.
        let ok_update = cordis::loader::EntryUpdate {
            config: Some(serde_json::json!({"v": 8})),
            ..Default::default()
        };
        let (status, Json(ok_body)) = patch_cordis_entry(
            State(ctx.clone()),
            Path("calc".into()),
            axum::Json(ok_update),
        )
        .await
        .expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ok_body["patched"], json!(true));
        assert!(ok_body.get("issues").is_none(), "success carries no issues");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// PATCH with a `parent` field MOVES the entry first, THEN applies any
    /// field updates in the same call. A live fiber keeps its identity across
    /// the rename (journal re-key preserves fiber id), and the config update
    /// lands under the NEW id driving that same fiber.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn patch_endpoint_moves_entry() {
        /// Second probe type so grp/svc never collide as duplicate providers.
        struct Anchor(std::sync::atomic::AtomicU64);
        impl ::cordis::Service for Anchor {}

        let initial_toml = "[[entry]]\nid = \"grp\"\nplugin = \"GroupMarker\"\ndisabled = false\n\n[entry.config]\n\
            [[entry]]\nid = \"svc\"\nplugin = \"CalculatorService\"\ndisabled = false\n\n[entry.config]\n";
        // CurrentEntries starts EMPTY so the boot-like reload below actually
        // Begins both entries and journals their live fibers.
        let (ctx, dir) = build_entries_fixture("patch-move", initial_toml, vec![]);
        // Register the second factory the group entry rides on.
        let registry = ctx.get::<cordis::PluginRegistry>().expect("registry");
        registry.register(
            "GroupMarker",
            Arc::new(|ctx: &Arc<::cordis::Context>, _cfg| {
                let fut = ctx.plugin(Anchor(std::sync::atomic::AtomicU64::new(0)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );

        // Boot-like first apply so both entries have journaled live fibers.
        let (status, Json(body)) = reload_cordis_entries(State(ctx.clone()))
            .await
            .expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert!(body["applied"].as_array().unwrap().len() >= 2);
        let journal = ctx.get::<cordis::LoaderJournal>().expect("journal");
        let svc_fid = journal.get("svc").unwrap().fiber_id.unwrap();

        // Move + reconfigure in one PATCH: svc becomes grp:svc with v = 9.
        let update = cordis::loader::EntryUpdate {
            config: Some(serde_json::json!({"v": 9})),
            parent: Some(Some("grp".into())),
            position: None,
            ..Default::default()
        };
        let (status, Json(body)) =
            patch_cordis_entry(State(ctx.clone()), Path("svc".into()), axum::Json(update))
                .await
                .expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["patched"], json!(true));
        assert_eq!(
            body["renamed"],
            json!([["svc", "grp:svc"]]),
            "rename pairs accompany the patch: {body}"
        );
        assert_eq!(body["entry"]["id"], "grp:svc");
        assert_eq!(body["entry"]["config"]["v"], 9);
        assert_eq!(
            body["entry"]["position"]["parent"], "grp",
            "post-patch entry carries its new parent pointer"
        );

        // Fiber identity survived the move; the update landed under the new id.
        assert_eq!(journal.get("grp:svc").unwrap().fiber_id, Some(svc_fid));
        assert_eq!(journal.get("grp:svc").unwrap().config, json!({"v": 9}));
        assert!(journal.get("svc").is_none(), "old journal key gone");
        assert!(
            ctx.get::<Probe>().is_some(),
            "live instance never disposed by the move"
        );

        // The program file persists the renamed entry with parent pointer…
        let raw = std::fs::read_to_string(dir.join("cordis-entries.toml")).expect("read back");
        assert!(raw.contains("id = \"grp:svc\""), "renamed on disk: {raw}");
        assert!(raw.contains("[entry.position]"), "parent persisted: {raw}");
        assert!(!raw.contains("id = \"svc\""), "stale id gone from disk");

        // …and a follow-up no-op PATCH round-trips cleanly (no phantom
        // retire/begin for the renamed ids).
        let noop = cordis::loader::EntryUpdate::default();
        let (status, Json(body)) =
            patch_cordis_entry(State(ctx.clone()), Path("grp:svc".into()), axum::Json(noop))
                .await
                .expect("resp");
        assert_eq!(status, StatusCode::OK);
        assert!(
            body["applied"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["action"] != "Retire" && a["action"] != "retire"),
            "no phantom retirement after move: {body}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An INVALID move via PATCH answers 409 and leaves both the file and
    /// the applied tree untouched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn patch_endpoint_invalid_move_conflicts_without_mutating() {
        let initial_toml = "[[entry]]\nid = \"a\"\nplugin = \"CalculatorService\"\ndisabled = false\n\n[entry.config]\n\
            [[entry]]\nid = \"b\"\nplugin = \"CalculatorService\"\ndisabled = false\n\n[entry.config]\n\
            [[entry]]\nid = \"b:a\"\nplugin = \"CalculatorService\"\ndisabled = false\n\n[entry.config]\n";
        let (ctx, dir) = build_entries_fixture(
            "patch-move-bad",
            initial_toml,
            vec![
                probe_entry("a", false),
                probe_entry("b", false),
                probe_entry("b:a", false),
            ],
        );
        let before = std::fs::read_to_string(dir.join("cordis-entries.toml")).expect("read");

        let update = cordis::loader::EntryUpdate {
            parent: Some(Some("b".into())), // would collide with existing b:a
            ..Default::default()
        };
        let (status, Json(body)) =
            patch_cordis_entry(State(ctx.clone()), Path("a".into()), axum::Json(update))
                .await
                .expect("resp");
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["patched"], json!(false));
        assert!(
            body["error"]
                .as_str()
                .map(|e| e.contains("already used"))
                .unwrap_or(false),
            "collision named: {body}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("cordis-entries.toml")).expect("read back"),
            before,
            "failed move must not touch the file"
        );

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
        fiber.push_undo_labeled(cordis::UndoMeta::new("provide:probe"), Box::new(|| {}));

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

    /// Services snapshot mirrors the undo-labels shape: one row per tracked
    /// fiber with state summary + disposed flag + pending undo count; the
    /// missing-registry path answers 503. A not-ready service (availability
    /// predicate `false`) rests its fiber as inspectable `Failed` and the
    /// snapshot surfaces both the state and the error message.
    #[tokio::test]
    async fn cordis_services_lists_tracked_fibers_with_state() {
        #[derive(Debug)]
        struct NotReadyProbe(u64);
        // Availability predicate `false` → registration rests Failed.
        impl ::cordis::Service for NotReadyProbe {
            fn check(&self) -> bool {
                false
            }
        }
        struct NotReadyPlugin;
        impl ::cordis::Plugin for NotReadyPlugin {
            type Config = ();
            type Provides = NotReadyProbe;
            fn apply(
                &self,
                _ctx: &Arc<Context>,
                _cfg: Self::Config,
            ) -> Result<Arc<NotReadyProbe>, ::cordis::CordisError> {
                Ok(Arc::new(NotReadyProbe(1)))
            }
        }
        #[derive(Debug)]
        struct ReadyProbe(u64);
        impl ::cordis::Service for ReadyProbe {}
        struct ReadyPlugin;
        impl ::cordis::Plugin for ReadyPlugin {
            type Config = ();
            type Provides = ReadyProbe;
            fn apply(
                &self,
                _ctx: &Arc<Context>,
                _cfg: Self::Config,
            ) -> Result<Arc<ReadyProbe>, ::cordis::CordisError> {
                Ok(Arc::new(ReadyProbe(2)))
            }
        }

        let ctx = Context::new_root();
        let registry = ctx.provide(RegistryService::new());
        let failed_fid = registry
            .register(&ctx, NotReadyPlugin, ())
            .expect("not-ready registration rests Failed without throwing");
        let fiber = registry.get_fiber(failed_fid).unwrap();

        let ok_fid = registry
            .register(&ctx, ReadyPlugin, ())
            .expect("healthy registration");

        let resp = list_cordis_services(State(ctx.clone()))
            .await
            .expect("handler");
        assert_eq!(resp.0, StatusCode::OK);
        let fibers = resp.1 .0["fibers"].as_array().unwrap();

        // Healthy sibling: Active, not disposed, error absent.
        let active = fibers
            .iter()
            .find(|f| f["fiber_id"] == json!(ok_fid))
            .expect("healthy fiber listed");
        assert!(
            active["state"]
                .as_str()
                .expect("state string")
                .starts_with("Active"),
            "healthy fiber rests Active, got {:?}",
            active["state"]
        );
        assert_eq!(active["disposed"], json!(false));
        assert_eq!(active["pending_undo_count"], json!(1));
        assert!(active["error"].is_null());

        // Failed fiber: state names Failed, carries the rejection message,
        // and counts the pending undos pushed onto it.
        let failed = fibers
            .iter()
            .find(|f| f["fiber_id"] == json!(failed_fid))
            .expect("failed fiber listed");
        assert!(
            failed["state"]
                .as_str()
                .expect("state string")
                .starts_with("Failed"),
            "state names Failed, got {:?}",
            failed["state"]
        );
        assert_eq!(
            failed["error"],
            json!("availability predicate rejected service")
        );
        assert_eq!(
            failed["pending_undo_count"],
            json!(fiber.pending_undo_labels().len()),
            "undo count mirrors the accumulator"
        );
        assert_eq!(failed["disposed"], json!(false));

        // 503 when no RegistryService is on the context.
        let bare = Context::new_root();
        let resp = list_cordis_services(State(bare)).await.expect("h");
        assert_eq!(resp.0, StatusCode::SERVICE_UNAVAILABLE);
    }

    /// The logs endpoint reads the process-global ring: empty before any
    /// install, and returning seeded records once a ring is installed.
    /// (The ring's trim-at-capacity behavior is unit-tested in
    /// `ares_llm::exporter`; this covers only the endpoint seam.)
    #[tokio::test]
    async fn cordis_logs_returns_records_once_seeded() {
        use ares_llm::observability::LlmCallRecord;

        // No ring installed yet → empty logs, still 200.
        if active_log_ring().is_none() {
            let resp = cordis_logs().await.expect("handler");
            assert_eq!(resp.0, StatusCode::OK);
            assert_eq!(
                resp.1 .0["logs"].as_array().unwrap().len(),
                0,
                "no ring installed means an empty log list"
            );
        }

        // Install a dedicated ring for this test; install_log_ring keeps the
        // FIRST ring in a process, so on a re-run this may observe another
        // test's ring — seed through whatever ring is in effect.
        let first = install_log_ring(Arc::new(ares_llm::exporter::LogRing::new(8)));
        first.push(LlmCallRecord {
            step_index: 0,
            provider: "openai".into(),
            model: "gpt-4o".into(),
            prompt_tokens: 10,
            completion_tokens: 5,
            latency_ms: 42,
            status: "success".into(),
            cached_tokens: None,
            total_time_ms: Some(43),
        });

        let resp = cordis_logs().await.expect("handler");
        assert_eq!(resp.0, StatusCode::OK);
        let logs = resp.1 .0["logs"].as_array().unwrap();
        assert!(!logs.is_empty(), "seeded ring must surface records");
        let ours = logs
            .iter()
            .rev()
            .find(|l| l["model"] == json!("gpt-4o") && l["provider"] == json!("openai"))
            .expect("seeded record present");
        assert_eq!(ours["prompt_tokens"], json!(10));
        assert_eq!(ours["completion_tokens"], json!(5));
        assert_eq!(ours["status"], json!("success"));
    }
}
