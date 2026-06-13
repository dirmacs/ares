//! Periodic health metrics aggregation job.
//!
//! Runs once per hour, querying `agent_runs` and `run_costs` for the last
//! hour and upserting aggregated per-tenant, per-agent health metrics into
//! `agent_health_metrics`.

use ares_db::run_history::{AgentHealthMetrics, ModelHealthMetrics, RunHistoryStore};
use rust_decimal::Decimal;
use sqlx::{PgPool, Row};
use std::time::Duration;
use tokio::time::interval;
use uuid::Uuid;

/// Spawn the health metrics aggregation background job.
///
/// The job sleeps for one hour between runs. On each tick it:
/// 1. Scans `agent_runs` for the past hour.
/// 2. Computes aggregations grouped by `(tenant_id, agent_name)`.
/// 3. Upserts rows into `agent_health_metrics`.
fn percent_rate(failed: i64, total: i64) -> Decimal {
    if total > 0 {
        Decimal::new(failed * 100, 0) / Decimal::new(total, 0)
    } else {
        Decimal::ZERO
    }
}

pub fn spawn(pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(3600));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            if let Err(e) = run_once(&pool).await {
                tracing::warn!(error = %e, "Health metrics aggregation failed");
            }
        }
    });
}

async fn run_once(
    pool: &PgPool,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let now = chrono::Utc::now().timestamp();
    let hour_ago = now - 3600;

    // Aggregate agent_runs for the last hour
    let rows = sqlx::query(
        "SELECT \
            tenant_id, \
            agent_name, \
            COUNT(*) AS total_runs, \
            COUNT(*) FILTER (WHERE status = 'completed') AS successful_runs, \
            COUNT(*) FILTER (WHERE status = 'failed') AS failed_runs, \
            COALESCE(AVG(duration_ms), 0)::bigint AS avg_latency_ms, \
            COALESCE(PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY duration_ms), 0)::bigint AS p50_latency_ms, \
            COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY duration_ms), 0)::bigint AS p95_latency_ms, \
            COALESCE(PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY duration_ms), 0)::bigint AS p99_latency_ms, \
            COALESCE(SUM(input_tokens + output_tokens), 0) AS total_tokens \
         FROM agent_runs \
         WHERE created_at >= $1 AND created_at <= $2 \
         GROUP BY tenant_id, agent_name",
    )
    .bind(hour_ago)
    .bind(now)
    .fetch_all(pool)
    .await?;

    let store = RunHistoryStore::new(pool);

    for row in rows {
        let tenant_id: String = row.try_get("tenant_id")?;
        let agent_name: String = row.try_get("agent_name")?;
        let total_runs: i64 = row.try_get("total_runs")?;
        let successful_runs: i64 = row.try_get("successful_runs")?;
        let failed_runs: i64 = row.try_get("failed_runs")?;
        let avg_latency_ms: i64 = row.try_get("avg_latency_ms")?;
        let p50_latency_ms: i64 = row.try_get("p50_latency_ms")?;
        let p95_latency_ms: i64 = row.try_get("p95_latency_ms")?;
        let p99_latency_ms: i64 = row.try_get("p99_latency_ms")?;
        let total_tokens: i64 = row.try_get("total_tokens")?;

        // Fetch total cost from run_costs for this tenant in the same window
        let total_cost_usd: Decimal = sqlx::query_scalar(
            "SELECT COALESCE(SUM(total_estimated_cost_usd), 0) FROM run_costs \
             WHERE tenant_id = $1 AND created_at >= $2 AND created_at <= $3",
        )
        .bind(&tenant_id)
        .bind(hour_ago)
        .bind(now)
        .fetch_one(pool)
        .await
        .unwrap_or(Decimal::ZERO);

        let error_rate_pct = percent_rate(failed_runs, total_runs);

        let metrics = AgentHealthMetrics {
            id: Uuid::new_v4().to_string(),
            tenant_id,
            agent_name,
            period_start: hour_ago,
            period_end: now,
            total_runs,
            successful_runs,
            failed_runs,
            avg_latency_ms,
            p50_latency_ms,
            p95_latency_ms,
            p99_latency_ms,
            total_tokens,
            total_cost_usd,
            error_rate_pct,
            created_at: now,
        };

        if let Err(e) = store.insert_health_metrics(&metrics).await {
            tracing::warn!(
                error = %e,
                tenant_id = %metrics.tenant_id,
                agent_name = %metrics.agent_name,
                "Failed to insert health metrics"
            );
        }
    }

    // Aggregate run_llm_calls for the last hour by (tenant_id, model)
    let model_rows = sqlx::query(
        "SELECT \
            tenant_id, \
            model, \
            COUNT(*) AS total_calls, \
            COUNT(*) FILTER (WHERE status = 'success') AS successful_calls, \
            COUNT(*) FILTER (WHERE status <> 'success') AS failed_calls, \
            COALESCE(AVG(latency_ms), 0)::bigint AS avg_latency_ms, \
            COALESCE(PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY latency_ms), 0)::bigint AS p50_latency_ms, \
            COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY latency_ms), 0)::bigint AS p95_latency_ms, \
            COALESCE(PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY latency_ms), 0)::bigint AS p99_latency_ms, \
            COALESCE(SUM(total_tokens), 0) AS total_tokens, \
            COALESCE(SUM(estimated_cost_usd), 0) AS total_cost_usd \
         FROM run_llm_calls \
         WHERE created_at >= $1 AND created_at <= $2 \
         GROUP BY tenant_id, model",
    )
    .bind(hour_ago)
    .bind(now)
    .fetch_all(pool)
    .await?;

    for row in model_rows {
        let tenant_id: String = row.try_get("tenant_id")?;
        let model: String = row.try_get("model")?;
        let total_calls: i64 = row.try_get("total_calls")?;
        let successful_calls: i64 = row.try_get("successful_calls")?;
        let failed_calls: i64 = row.try_get("failed_calls")?;
        let avg_latency_ms: i64 = row.try_get("avg_latency_ms")?;
        let p50_latency_ms: i64 = row.try_get("p50_latency_ms")?;
        let p95_latency_ms: i64 = row.try_get("p95_latency_ms")?;
        let p99_latency_ms: i64 = row.try_get("p99_latency_ms")?;
        let total_tokens: i64 = row.try_get("total_tokens")?;

        let total_cost_usd: Decimal = row.try_get("total_cost_usd")?;
        let error_rate_pct = percent_rate(failed_calls, total_calls);

        let metrics = ModelHealthMetrics {
            id: Uuid::new_v4().to_string(),
            tenant_id,
            model,
            period_start: hour_ago,
            period_end: now,
            total_calls,
            successful_calls,
            failed_calls,
            avg_latency_ms,
            p50_latency_ms,
            p95_latency_ms,
            p99_latency_ms,
            total_tokens,
            total_cost_usd,
            error_rate_pct,
            created_at: now,
        };

        if let Err(e) = store.insert_model_health_metrics(&metrics).await {
            tracing::warn!(
                error = %e,
                tenant_id = %metrics.tenant_id,
                model = %metrics.model,
                "Failed to insert model health metrics"
            );
        }
    }

    tracing::info!("Health metrics aggregation complete for the last hour");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::percent_rate;
    use rust_decimal::Decimal;

    #[test]
    fn percent_rate_returns_percentage_not_fraction() {
        assert_eq!(percent_rate(1, 4), Decimal::new(25, 0));
        assert_eq!(percent_rate(0, 4), Decimal::ZERO);
        assert_eq!(percent_rate(1, 0), Decimal::ZERO);
    }
}
