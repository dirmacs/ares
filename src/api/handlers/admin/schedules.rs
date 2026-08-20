//! Admin schedules domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).



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

pub async fn list_schedules(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<db_schedules::AgentSchedule>>> {
    let tenant_id = params.get("tenant_id").map(|s| s.as_str()).unwrap_or("");
    if tenant_id.is_empty() {
        return Err(AppError::InvalidInput(
            "tenant_id query param is required".into(),
        ));
    }
    let store = db_schedules::ScheduleStore::new(state.tenant_db.pool());
    let schedules = store.list_schedules(tenant_id).await?;
    Ok(Json(schedules))
}

pub async fn list_schedule_missed_runs(
    State(state): State<AppState>,
    Path((tenant_id, schedule_id)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<db_schedules::MissedRunAudit>>> {
    let limit = params
        .get("limit")
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(10)
        .clamp(1, 100);
    let store = db_schedules::ScheduleStore::new(state.tenant_db.pool());
    let audits = store
        .list_missed_runs_for_tenant(&tenant_id, &schedule_id, limit)
        .await?;
    Ok(Json(audits))
}

pub async fn create_schedule(
    State(state): State<AppState>,
    Json(req): Json<db_schedules::CreateScheduleRequest>,
) -> Result<Json<db_schedules::AgentSchedule>> {
    let store = db_schedules::ScheduleStore::new(state.tenant_db.pool());
    let schedule = store.create_schedule(&req).await?;

    let pool = state.tenant_db.pool().clone();
    let t_id = schedule.tenant_id.clone();
    let a_name = schedule.agent_name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "schedule_create",
            "agent_schedule",
            &a_name,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(Json(schedule))
}

pub async fn update_schedule(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<db_schedules::CreateScheduleRequest>,
) -> Result<Json<db_schedules::AgentSchedule>> {
    let store = db_schedules::ScheduleStore::new(state.tenant_db.pool());
    let schedule = store
        .update_schedule(&id, &req)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("schedule {id} not found")))?;

    let pool = state.tenant_db.pool().clone();
    let t_id = schedule.tenant_id.clone();
    let a_name = schedule.agent_name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "schedule_update",
            "agent_schedule",
            &a_name,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(Json(schedule))
}

pub async fn delete_schedule(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let store = db_schedules::ScheduleStore::new(state.tenant_db.pool());
    let rows = store.delete_schedule(&id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!("schedule {id} not found")));
    }

    let pool = state.tenant_db.pool().clone();
    let sid = id.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "schedule_delete",
            "agent_schedule",
            &sid,
            None,
            None,
        )
        .await;
    });

    Ok(StatusCode::NO_CONTENT)
}

pub async fn update_tenant_schedule(
    State(state): State<AppState>,
    Path((tenant_id, id)): Path<(String, String)>,
    Json(mut req): Json<db_schedules::CreateScheduleRequest>,
) -> Result<Json<db_schedules::AgentSchedule>> {
    req.tenant_id = tenant_id.clone();
    update_schedule(State(state), Path(id), Json(req)).await
}

pub async fn delete_tenant_schedule(
    State(state): State<AppState>,
    Path((tenant_id, id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let store = db_schedules::ScheduleStore::new(state.tenant_db.pool());
    let rows = store.delete_schedule_for_tenant(&tenant_id, &id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "schedule {id} not found for tenant {tenant_id}"
        )));
    }

    let pool = state.tenant_db.pool().clone();
    let sid = id.clone();
    let t_id = tenant_id.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "schedule_delete",
            "agent_schedule",
            &sid,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(StatusCode::NO_CONTENT)
}

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{delete, get, post, put};
    axum::Router::new()
        .route("/schedules/list_schedules", get(list_schedules))
        .route("/schedules/list_schedule_missed_runs", get(list_schedule_missed_runs))
        .route("/schedules/create_schedule", post(create_schedule))
        .route("/schedules/update_schedule", put(update_schedule))
        .route("/schedules/delete_schedule", delete(delete_schedule))
        .route("/schedules/update_tenant_schedule", put(update_tenant_schedule))
        .route("/schedules/delete_tenant_schedule", delete(delete_tenant_schedule))
}

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ares_cordis_core::Service;
pub struct AdminSchedulesService;
impl Service for AdminSchedulesService {}