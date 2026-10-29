//! Fire-and-forget run-cost aggregation into `run_costs` via `ares-store`.

use ares_store::run_history::RunHistoryStore;
use sqlx::PgPool;

/// Spawn aggregation for a completed run (same semantics as HTTP handlers).
pub fn spawn_aggregate_run_cost(
    pool: PgPool,
    run_id: String,
    tenant_id: String,
    agent_name: String,
    duration_ms: i64,
) {
    tokio::spawn(async move {
        let store = RunHistoryStore::new(&pool);
        if let Err(e) = store
            .aggregate_run_cost(&run_id, &tenant_id, &agent_name, duration_ms)
            .await
        {
            e.log(&run_id);
        }
    });
}
