//! Admin triggers domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).



use std::sync::Arc;
use ares_cordis_core::Context;
use crate::AppState;
use crate::db::audit_log;
use crate::db::schedules as db_schedules;
use crate::types::{AppError, Result};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use sha2::Digest;
use std::collections::HashMap;

pub async fn list_triggers(
    State(ctx): State<Arc<Context>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<db_schedules::EventTrigger>>> {
    let tenant_id = params.get("tenant_id").map(|s| s.as_str()).unwrap_or("");
    if tenant_id.is_empty() {
        return Err(AppError::InvalidInput(
            "tenant_id query param is required".into(),
        ));
    }
    let __pool_1 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_schedules::EventTriggerStore::new(&__pool_1);
    let triggers = store.list_triggers(tenant_id).await?;
    Ok(Json(triggers))
}

pub async fn create_trigger(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<db_schedules::CreateTriggerRequest>,
) -> Result<Json<db_schedules::EventTrigger>> {
    let __pool_2 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_schedules::EventTriggerStore::new(&__pool_2);
    let trigger = store.create_trigger(&req).await?;

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let t_id = trigger.tenant_id.clone();
    let tr_name = trigger.name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "trigger_create",
            "event_trigger",
            &tr_name,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(Json(trigger))
}

pub async fn delete_trigger(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let __pool_3 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_schedules::EventTriggerStore::new(&__pool_3);
    let rows = store.delete_trigger(&id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!("trigger {id} not found")));
    }

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let tid = id.clone();
    tokio::spawn(async move {
        let _ =
            audit_log::log_admin_action(&pool, "trigger_delete", "event_trigger", &tid, None, None)
                .await;
    });

    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_tenant_triggers(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<db_schedules::EventTrigger>>> {
    let __pool_4 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_schedules::EventTriggerStore::new(&__pool_4);
    let triggers = store.list_triggers(&tenant_id).await?;
    Ok(Json(triggers))
}

pub async fn create_tenant_trigger(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    Json(mut req): Json<db_schedules::CreateTriggerRequest>,
) -> Result<Json<db_schedules::EventTrigger>> {
    req.tenant_id = tenant_id;
    let __pool_5 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_schedules::EventTriggerStore::new(&__pool_5);
    let trigger = store.create_trigger(&req).await?;

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let t_id = trigger.tenant_id.clone();
    let tr_name = trigger.name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "trigger_create",
            "event_trigger",
            &tr_name,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(Json(trigger))
}

pub async fn update_tenant_trigger(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, id)): Path<(String, String)>,
    Json(mut req): Json<db_schedules::CreateTriggerRequest>,
) -> Result<Json<db_schedules::EventTrigger>> {
    req.tenant_id = tenant_id.clone();
    let __pool_6 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_schedules::EventTriggerStore::new(&__pool_6);
    let trigger = store
        .update_trigger(&tenant_id, &id, &req)
        .await?
        .ok_or_else(|| {
            AppError::NotFound(format!("trigger {id} not found for tenant {tenant_id}"))
        })?;

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let t_id = trigger.tenant_id.clone();
    let tr_name = trigger.name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "trigger_update",
            "event_trigger",
            &tr_name,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(Json(trigger))
}

pub async fn delete_tenant_trigger(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let __pool_7 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_schedules::EventTriggerStore::new(&__pool_7);
    let rows = store.delete_trigger_for_tenant(&tenant_id, &id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "trigger {id} not found for tenant {tenant_id}"
        )));
    }

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "trigger_delete",
            "event_trigger",
            &id,
            Some(&tenant_id),
            None,
        )
        .await;
    });

    Ok(StatusCode::NO_CONTENT)
}

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{delete, get, post, put};
    axum::Router::new()
        .route("/triggers/list_triggers", get(list_triggers))
        .route("/triggers/create_trigger", post(create_trigger))
        .route("/triggers/delete_trigger", delete(delete_trigger))
        .route("/triggers/list_tenant_triggers", get(list_tenant_triggers))
        .route("/triggers/create_tenant_trigger", post(create_tenant_trigger))
        .route("/triggers/update_tenant_trigger", put(update_tenant_trigger))
        .route("/triggers/delete_tenant_trigger", delete(delete_tenant_trigger))
}

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ares_cordis_core::Service;
pub struct AdminTriggersService;
impl Service for AdminTriggersService {}
