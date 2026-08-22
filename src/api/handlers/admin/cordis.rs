//! Cordis service-lifecycle domain — runtime retire / re-provide of services.
//!
//! Complements the reactive-fiber demo wired in `run_server`: retiring
//! `EventsService` flips dependent fibers (demo fid 990001) to `Inactive`;
//! re-providing flips them back to `Active`. Only DIRECT-provided concrete
//! types are retirably supported today; wrapper services
//! (`crate::context_services::*Service`) hold an inner `Arc<T>` under a
//! distinct TypeId, so removal would not cascade — those answer 409.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use ares_cordis_core::{Context, EventsService, ReflectService};
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
) -> Result<(StatusCode, Json<serde_json::Value>), crate::types::AppError> {
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
                     services such as ToolRegistryService are not supported today"
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
) -> Result<(StatusCode, Json<serde_json::Value>), crate::types::AppError> {
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
        other => Err(crate::types::AppError::InvalidInput(format!(
            "service {other} cannot be provided dynamically: only direct Cordis \
             services with known constructors are supported today"
        ))),
    }
}

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::post;
    axum::Router::new()
        .route("/cordis/services/{name}/retire", post(retire_cordis_service))
        .route("/cordis/services/{name}/provide", post(provide_cordis_service))
}

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ares_cordis_core::Service;
pub struct AdminCordisService;
impl Service for AdminCordisService {}

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

        let fiber = Arc::new(ares_cordis_core::Fiber::new());
        fiber.declare_inject::<EventsService>();
        let fid: u64 = 990_002;
        reflect.register_dependent(TypeId::of::<EventsService>(), fid);
        reflect.register_fiber(fid, fiber.clone(), TypeId::of::<EventsService>());
        fiber.refresh(&ctx).await;
        assert!(matches!(fiber.state(), ares_cordis_core::FiberState::Active { .. }));

        retire_cordis_service(State(ctx.clone()), Path("events_service".into()))
            .await
            .expect("handler");
        // remove() notified dependents; give the spawned refresh a beat.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(matches!(
            fiber.state(),
            ares_cordis_core::FiberState::Inactive { .. }
        ));

        provide_cordis_service(State(ctx.clone()), Path("events_service".into()))
            .await
            .expect("handler");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(matches!(fiber.state(), ares_cordis_core::FiberState::Active { .. }));
    }
}
