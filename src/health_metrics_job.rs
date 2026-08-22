//! Periodic health metrics aggregation job.
//!
//! Runs once per hour, querying `agent_runs` and `run_costs` for the last
//! hour and upserting aggregated per-tenant, per-agent health metrics into
//! `agent_health_metrics`.

use ares_store::run_history::{AgentHealthMetrics, ModelHealthMetrics, RunHistoryStore};
use rust_decimal::Decimal;
use sqlx::{PgPool, Row, postgres::PgRow};
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

fn agent_cost_sql() -> &'static str {
    "SELECT COALESCE(SUM(total_estimated_cost_usd), 0) FROM run_costs \
     WHERE tenant_id = $1 AND agent_name = $2 AND created_at >= $3 AND created_at <= $4"
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
    let period_end = chrono::Utc::now().timestamp();
    let period_start = period_end - 3600;
    let store = RunHistoryStore::new(pool);

    aggregate_agent_health_metrics(pool, &store, period_start, period_end).await?;
    aggregate_model_health_metrics(pool, &store, period_start, period_end).await?;

    tracing::info!("Health metrics aggregation complete for the last hour");
    Ok(())
}

async fn aggregate_agent_health_metrics(
    pool: &PgPool,
    store: &RunHistoryStore<'_>,
    period_start: i64,
    period_end: i64,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let rows = sqlx::query(agent_health_sql())
        .bind(period_start)
        .bind(period_end)
        .fetch_all(pool)
        .await?;

    for row in rows {
        let tenant_id: String = row.try_get("tenant_id")?;
        let agent_name: String = row.try_get("agent_name")?;
        let total_cost_usd =
            fetch_agent_cost(pool, &tenant_id, &agent_name, period_start, period_end).await;
        let metrics = agent_metrics_from_row(row, period_start, period_end, total_cost_usd)?;
        if let Err(e) = store.insert_health_metrics(&metrics).await {
            tracing::warn!(
                error = %e,
                tenant_id = %metrics.tenant_id,
                agent_name = %metrics.agent_name,
                "Failed to insert health metrics"
            );
        }
    }

    Ok(())
}

async fn aggregate_model_health_metrics(
    pool: &PgPool,
    store: &RunHistoryStore<'_>,
    period_start: i64,
    period_end: i64,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let rows = sqlx::query(model_health_sql())
        .bind(period_start)
        .bind(period_end)
        .fetch_all(pool)
        .await?;

    for row in rows {
        let metrics = model_metrics_from_row(row, period_start, period_end)?;
        if let Err(e) = store.insert_model_health_metrics(&metrics).await {
            tracing::warn!(
                error = %e,
                tenant_id = %metrics.tenant_id,
                model = %metrics.model,
                "Failed to insert model health metrics"
            );
        }
    }

    Ok(())
}

async fn fetch_agent_cost(
    pool: &PgPool,
    tenant_id: &str,
    agent_name: &str,
    period_start: i64,
    period_end: i64,
) -> Decimal {
    sqlx::query_scalar(agent_cost_sql())
        .bind(tenant_id)
        .bind(agent_name)
        .bind(period_start)
        .bind(period_end)
        .fetch_one(pool)
        .await
        .unwrap_or(Decimal::ZERO)
}

fn agent_metrics_from_row(
    row: PgRow,
    period_start: i64,
    period_end: i64,
    total_cost_usd: Decimal,
) -> sqlx::Result<AgentHealthMetrics> {
    let total_runs = row.try_get("total_runs")?;
    let failed_runs = row.try_get("failed_runs")?;
    Ok(AgentHealthMetrics {
        id: Uuid::new_v4().to_string(),
        tenant_id: row.try_get("tenant_id")?,
        agent_name: row.try_get("agent_name")?,
        period_start,
        period_end,
        total_runs,
        successful_runs: row.try_get("successful_runs")?,
        failed_runs,
        avg_latency_ms: row.try_get("avg_latency_ms")?,
        p50_latency_ms: row.try_get("p50_latency_ms")?,
        p95_latency_ms: row.try_get("p95_latency_ms")?,
        p99_latency_ms: row.try_get("p99_latency_ms")?,
        total_tokens: row.try_get("total_tokens")?,
        total_cost_usd,
        error_rate_pct: percent_rate(failed_runs, total_runs),
        created_at: period_end,
    })
}

fn model_metrics_from_row(
    row: PgRow,
    period_start: i64,
    period_end: i64,
) -> sqlx::Result<ModelHealthMetrics> {
    let total_calls = row.try_get("total_calls")?;
    let failed_calls = row.try_get("failed_calls")?;
    Ok(ModelHealthMetrics {
        id: Uuid::new_v4().to_string(),
        tenant_id: row.try_get("tenant_id")?,
        model: row.try_get("model")?,
        period_start,
        period_end,
        total_calls,
        successful_calls: row.try_get("successful_calls")?,
        failed_calls,
        avg_latency_ms: row.try_get("avg_latency_ms")?,
        p50_latency_ms: row.try_get("p50_latency_ms")?,
        p95_latency_ms: row.try_get("p95_latency_ms")?,
        p99_latency_ms: row.try_get("p99_latency_ms")?,
        total_tokens: row.try_get("total_tokens")?,
        total_cost_usd: row.try_get("total_cost_usd")?,
        error_rate_pct: percent_rate(failed_calls, total_calls),
        created_at: period_end,
    })
}

fn agent_health_sql() -> &'static str {
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
        COALESCE(SUM(input_tokens + output_tokens)::bigint, 0) AS total_tokens \
     FROM agent_runs \
     WHERE created_at >= $1 AND created_at <= $2 \
     GROUP BY tenant_id, agent_name"
}

fn model_health_sql() -> &'static str {
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
        COALESCE(SUM(total_tokens)::bigint, 0) AS total_tokens, \
        COALESCE(SUM(estimated_cost_usd), 0) AS total_cost_usd \
     FROM run_llm_calls \
     WHERE created_at >= $1 AND created_at <= $2 \
     GROUP BY tenant_id, model"
}

#[cfg(test)]
mod tests {
    use super::{agent_cost_sql, agent_health_sql, model_health_sql, percent_rate};
    use rust_decimal::Decimal;

    #[test]
    fn percent_rate_returns_percentage_not_fraction() {
        assert_eq!(percent_rate(1, 4), Decimal::new(25, 0));
        assert_eq!(percent_rate(0, 4), Decimal::ZERO);
        assert_eq!(percent_rate(1, 0), Decimal::ZERO);
    }

    #[test]
    fn agent_health_sql_groups_agent_window() {
        let sql = agent_health_sql();
        assert!(sql.contains("FROM agent_runs"));
        assert!(sql.contains("created_at >= $1"));
        assert!(sql.contains("created_at <= $2"));
        assert!(sql.contains("SUM(input_tokens + output_tokens)::bigint"));
        assert!(sql.contains("GROUP BY tenant_id, agent_name"));
    }

    #[test]
    fn model_health_sql_groups_model_window() {
        let sql = model_health_sql();
        assert!(sql.contains("FROM run_llm_calls"));
        assert!(sql.contains("created_at >= $1"));
        assert!(sql.contains("created_at <= $2"));
        assert!(sql.contains("SUM(total_tokens)::bigint"));
        assert!(sql.contains("GROUP BY tenant_id, model"));
    }

    #[test]
    fn agent_cost_sql_filters_to_agent_window() {
        let sql = agent_cost_sql();
        assert!(sql.contains("tenant_id = $1"));
        assert!(sql.contains("agent_name = $2"));
        assert!(sql.contains("created_at >= $3"));
        assert!(sql.contains("created_at <= $4"));
    }
}
