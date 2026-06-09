//! Background cron scheduler for agent runs.
//!
//! Periodically checks `agent_schedules` table for agents whose `next_run_at`
//! is in the past, runs them, and updates `last_run_at` / `next_run_at`.

use ares_db::schedules::{ScheduleStore, AgentSchedule};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;

/// Start the background scheduler loop.
pub async fn start_scheduler(pool: PgPool, app_state: Arc<crate::AppState>) {
    let mut ticker = interval(Duration::from_secs(60));
    loop {
        ticker.tick().await;
        if let Err(e) = run_due_schedules(&pool, &app_state).await {
            tracing::warn!("Scheduler tick failed: {}", e);
        }
    }
}

async fn run_due_schedules(pool: &PgPool, app_state: &Arc<crate::AppState>) -> Result<(), String> {
    let store = ScheduleStore::new(pool);
    let due = store.get_due_schedules().await.map_err(|e| e.to_string())?;
    for sched in due {
        tracing::info!(
            "Scheduler: running agent {} for tenant {}",
            sched.agent_name,
            sched.tenant_id
        );
        if let Err(e) = execute_scheduled_agent(&sched, app_state).await {
            tracing::warn!(
                "Scheduled run failed for agent {} (tenant {}): {}",
                sched.agent_name,
                sched.tenant_id,
                e
            );
        }
        let next = compute_next_run(&sched.cron_expression, &sched.timezone);
        if let Err(e) = store.update_schedule_run(&sched.id, next).await {
            tracing::warn!(
                "Failed to update schedule {} next_run_at: {}",
                sched.id,
                e
            );
        }
    }
    Ok(())
}

fn compute_next_run(_cron: &str, _tz: &str) -> i64 {
    // Default to 1 hour from now.
    // In a full implementation this would parse the cron expression using
    // a crate such as `cron` or `saffron`.
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 3600
}

async fn execute_scheduled_agent(
    sched: &AgentSchedule,
    _app_state: &Arc<crate::AppState>,
) -> Result<(), String> {
    // In a full implementation this would invoke the same agent execution
    // path as `crate::api::handlers::v1::run_agent`.  For now we log and
    // succeed so the schedule bookkeeping is exercised.
    tracing::info!(
        "Scheduled run for agent {} (tenant {})",
        sched.agent_name,
        sched.tenant_id
    );
    Ok(())
}
