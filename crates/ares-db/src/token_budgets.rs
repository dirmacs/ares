//! Per-tenant LLM token budget tracking.
//!
//! Provides CRUD for `tenant_token_budgets` and `token_usage_log`
//! tables (migration 024).

use ares_types::types::{AppError, Result};
use chrono::{Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

// =============================================================================
// Structs
// =============================================================================

/// One persisted row in `tenant_token_budgets`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenBudget {
    pub id: String,
    pub tenant_id: String,
    pub period: String,
    pub token_limit: i64,
    pub tokens_used: i64,
    pub period_start: i64,
    pub period_end: i64,
    pub alert_threshold: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One persisted row in `token_usage_log`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenUsageEntry {
    pub id: String,
    pub tenant_id: String,
    pub run_id: Option<String>,
    pub agent_name: Option<String>,
    pub model: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub created_at: i64,
}

/// Derived budget status for a tenant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BudgetStatus {
    pub tenant_id: String,
    pub token_limit: i64,
    pub tokens_used: i64,
    pub remaining: i64,
    pub percentage: i64,
    pub alert_threshold: i64,
    pub would_exceed: bool,
}

impl BudgetStatus {
    /// Check whether an additional `estimated` tokens would push the budget over.
    pub fn would_exceed_with(&self, estimated: i64) -> bool {
        self.tokens_used + estimated > self.token_limit
    }
}

// =============================================================================
// Store
// =============================================================================

/// CRUD for token budget tables.
pub struct TokenBudgetStore<'a> {
    pool: &'a PgPool,
}

impl<'a> TokenBudgetStore<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// Get the budget for a tenant, or create a default one if missing.
    pub async fn get_or_create(
        &self,
        tenant_id: &str,
        period: &str,
        token_limit: i64,
    ) -> Result<TokenBudget> {
        validate_token_limit(token_limit)?;
        validate_period(period)?;

        if let Some(budget) = self.get(tenant_id).await? {
            return Ok(budget);
        }
        let now = Utc::now();
        let (period_start, period_end) = compute_period_bounds(period, now);
        let created_at = now.timestamp();
        let id = uuid::Uuid::new_v4().to_string();

        let row = sqlx::query(
            r#"
            INSERT INTO tenant_token_budgets
                (id, tenant_id, period, token_limit, tokens_used, period_start, period_end, alert_threshold, created_at, updated_at)
            VALUES ($1, $2, $3, $4, 0, $5, $6, 80, $7, $7)
            ON CONFLICT (tenant_id) DO NOTHING
            RETURNING id, tenant_id, period, token_limit, tokens_used, period_start, period_end, alert_threshold, created_at, updated_at
            "#,
        )
        .bind(&id)
        .bind(tenant_id)
        .bind(period)
        .bind(token_limit)
        .bind(period_start)
        .bind(period_end)
        .bind(created_at)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        if let Some(row) = row {
            Ok(row_to_budget(&row))
        } else {
            self.get(tenant_id).await?.ok_or_else(|| {
                AppError::Internal("Failed to get or create token budget".to_string())
            })
        }
    }

    /// Upsert a tenant budget.
    pub async fn set_budget(
        &self,
        tenant_id: &str,
        token_limit: i64,
        period: &str,
    ) -> Result<TokenBudget> {
        validate_token_limit(token_limit)?;
        validate_period(period)?;

        let now = Utc::now();
        let (period_start, period_end) = compute_period_bounds(period, now);
        let updated_at = now.timestamp();
        let id = uuid::Uuid::new_v4().to_string();

        let row = sqlx::query(
            r#"
            INSERT INTO tenant_token_budgets
                (id, tenant_id, period, token_limit, tokens_used, period_start, period_end, alert_threshold, created_at, updated_at)
            VALUES ($1, $2, $3, $4, 0, $5, $6, 80, $7, $7)
            ON CONFLICT (tenant_id) DO UPDATE SET
                period = EXCLUDED.period,
                token_limit = EXCLUDED.token_limit,
                period_start = EXCLUDED.period_start,
                period_end = EXCLUDED.period_end,
                updated_at = EXCLUDED.updated_at
            RETURNING id, tenant_id, period, token_limit, tokens_used, period_start, period_end, alert_threshold, created_at, updated_at
            "#,
        )
        .bind(&id)
        .bind(tenant_id)
        .bind(period)
        .bind(token_limit)
        .bind(period_start)
        .bind(period_end)
        .bind(updated_at)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(row_to_budget(&row))
    }

    /// Record LLM token usage and increment the tenant budget.
    pub async fn record_usage(
        &self,
        tenant_id: &str,
        run_id: Option<&str>,
        agent_name: &str,
        model: &str,
        input_tokens: i64,
        output_tokens: i64,
    ) -> Result<()> {
        self.roll_over_if_expired(tenant_id).await?;

        let total = input_tokens + output_tokens;
        let now = Utc::now().timestamp();

        let mut tx = self.pool.begin().await.map_err(sqlx_err)?;

        sqlx::query(
            "UPDATE tenant_token_budgets SET tokens_used = tokens_used + $1, updated_at = $2 WHERE tenant_id = $3",
        )
        .bind(total)
        .bind(now)
        .bind(tenant_id)
        .execute(&mut *tx)
        .await
        .map_err(sqlx_err)?;

        sqlx::query(
            r#"
            INSERT INTO token_usage_log
                (id, tenant_id, run_id, agent_name, model, input_tokens, output_tokens, total_tokens, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(tenant_id)
        .bind(run_id)
        .bind(agent_name)
        .bind(model)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(total)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(sqlx_err)?;

        tx.commit().await.map_err(sqlx_err)?;
        Ok(())
    }

    /// Check the current budget status for a tenant.
    pub async fn check_budget(&self, tenant_id: &str) -> Result<BudgetStatus> {
        self.roll_over_if_expired(tenant_id).await?;

        match self.get(tenant_id).await? {
            Some(b) => {
                let remaining = b.token_limit - b.tokens_used;
                let percentage = if b.token_limit > 0 {
                    (b.tokens_used * 100) / b.token_limit
                } else {
                    0
                };
                Ok(BudgetStatus {
                    tenant_id: b.tenant_id,
                    token_limit: b.token_limit,
                    tokens_used: b.tokens_used,
                    remaining: remaining.max(0),
                    percentage,
                    alert_threshold: b.alert_threshold,
                    would_exceed: b.tokens_used >= b.token_limit,
                })
            }
            None => Ok(BudgetStatus {
                tenant_id: tenant_id.to_string(),
                token_limit: 0,
                tokens_used: 0,
                remaining: 0,
                percentage: 0,
                alert_threshold: 80,
                would_exceed: false,
            }),
        }
    }

    /// Reset usage and roll period boundaries forward for a tenant.
    pub async fn reset_period(&self, tenant_id: &str) -> Result<()> {
        if let Some(b) = self.get(tenant_id).await? {
            let now = Utc::now();
            let (period_start, period_end) = compute_period_bounds(&b.period, now);
            let updated_at = now.timestamp();

            sqlx::query(
                "UPDATE tenant_token_budgets SET tokens_used = 0, period_start = $1, period_end = $2, updated_at = $3 WHERE tenant_id = $4",
            )
            .bind(period_start)
            .bind(period_end)
            .bind(updated_at)
            .bind(tenant_id)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;
        }
        Ok(())
    }

    /// Fetch a tenant budget without creating a default row.
    pub async fn get_budget(&self, tenant_id: &str) -> Result<Option<TokenBudget>> {
        self.roll_over_if_expired(tenant_id).await?;
        self.get(tenant_id).await
    }

    /// List recent token usage entries for a tenant.
    pub async fn list_usage(&self, tenant_id: &str, limit: i64) -> Result<Vec<TokenUsageEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT id, tenant_id, run_id, agent_name, model, input_tokens, output_tokens, total_tokens, created_at
            FROM token_usage_log
            WHERE tenant_id = $1
            ORDER BY created_at DESC
            LIMIT $2
            "#,
        )
        .bind(tenant_id)
        .bind(limit)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(rows.iter().map(row_to_usage).collect())
    }

    // -------------------------------------------------------------------------
    // Internal helpers
    // -------------------------------------------------------------------------

    async fn roll_over_if_expired(&self, tenant_id: &str) -> Result<()> {
        let Some(budget) = self.get(tenant_id).await? else {
            return Ok(());
        };
        let now = Utc::now();
        if now.timestamp() <= budget.period_end {
            return Ok(());
        }
        let (period_start, period_end) = compute_period_bounds(&budget.period, now);
        let updated_at = now.timestamp();
        sqlx::query(
            "UPDATE tenant_token_budgets SET tokens_used = 0, period_start = $1, period_end = $2, updated_at = $3 WHERE tenant_id = $4",
        )
        .bind(period_start)
        .bind(period_end)
        .bind(updated_at)
        .bind(tenant_id)
        .execute(self.pool)
        .await
        .map_err(sqlx_err)?;
        Ok(())
    }

    async fn get(&self, tenant_id: &str) -> Result<Option<TokenBudget>> {
        let row = sqlx::query(
            r#"
            SELECT id, tenant_id, period, token_limit, tokens_used, period_start, period_end, alert_threshold, created_at, updated_at
            FROM tenant_token_budgets
            WHERE tenant_id = $1
            "#,
        )
        .bind(tenant_id)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(row.map(|r| row_to_budget(&r)))
    }
}

// =============================================================================
// Row mappers
// =============================================================================

fn row_to_budget(row: &sqlx::postgres::PgRow) -> TokenBudget {
    TokenBudget {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        period: row.get("period"),
        token_limit: row.get("token_limit"),
        tokens_used: row.get("tokens_used"),
        period_start: row.get("period_start"),
        period_end: row.get("period_end"),
        alert_threshold: row.get("alert_threshold"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn row_to_usage(row: &sqlx::postgres::PgRow) -> TokenUsageEntry {
    TokenUsageEntry {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        run_id: row.get("run_id"),
        agent_name: row.get("agent_name"),
        model: row.get("model"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        total_tokens: row.get("total_tokens"),
        created_at: row.get("created_at"),
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn sqlx_err(e: sqlx::Error) -> AppError {
    AppError::Database(e.to_string())
}

fn validate_token_limit(token_limit: i64) -> Result<()> {
    if token_limit < 0 {
        return Err(AppError::InvalidInput(
            "token_limit must not be negative".to_string(),
        ));
    }
    Ok(())
}

fn validate_period(period: &str) -> Result<()> {
    match period {
        "daily" | "weekly" | "monthly" => Ok(()),
        _ => Err(AppError::InvalidInput(
            "period must be daily, weekly, or monthly".to_string(),
        )),
    }
}

fn compute_period_bounds(period: &str, now: chrono::DateTime<Utc>) -> (i64, i64) {
    match period {
        "daily" => {
            let start = now.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
            let end = start + chrono::Duration::days(1);
            (start.timestamp(), end.timestamp())
        }
        "weekly" => {
            let days_since_monday = now.date_naive().weekday().num_days_from_monday() as i64;
            let start_naive = now.date_naive() - chrono::Duration::days(days_since_monday);
            let start = start_naive.and_hms_opt(0, 0, 0).unwrap().and_utc();
            let end = start + chrono::Duration::days(7);
            (start.timestamp(), end.timestamp())
        }
        _ => {
            // monthly (default)
            let year = now.year();
            let month = now.month();
            let (next_year, next_month) = if month == 12 {
                (year + 1, 1)
            } else {
                (year, month + 1)
            };
            let start_naive = chrono::NaiveDate::from_ymd_opt(year, month, 1).unwrap();
            let end_naive = chrono::NaiveDate::from_ymd_opt(next_year, next_month, 1).unwrap();
            let start = start_naive.and_hms_opt(0, 0, 0).unwrap().and_utc();
            let end = end_naive.and_hms_opt(0, 0, 0).unwrap().and_utc();
            (start.timestamp(), end.timestamp())
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_budget_serde_roundtrip() {
        let original = TokenBudget {
            id: "budget-1".into(),
            tenant_id: "tenant-a".into(),
            period: "monthly".into(),
            token_limit: 1_000_000,
            tokens_used: 123_456,
            period_start: 1_700_000_000,
            period_end: 1_700_086_400,
            alert_threshold: 80,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_100,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let back: TokenBudget = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn token_usage_entry_serde_roundtrip() {
        let original = TokenUsageEntry {
            id: "usage-1".into(),
            tenant_id: "tenant-a".into(),
            run_id: Some("run-42".into()),
            agent_name: Some("product".into()),
            model: Some("gpt-4o".into()),
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            created_at: 1_700_000_000,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let back: TokenUsageEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn budget_status_would_exceed() {
        let status = BudgetStatus {
            tenant_id: "tenant-a".into(),
            token_limit: 100,
            tokens_used: 80,
            remaining: 20,
            percentage: 80,
            alert_threshold: 80,
            would_exceed: false,
        };
        assert!(!status.would_exceed_with(20));
        assert!(status.would_exceed_with(21));
    }

    #[test]
    fn budget_status_exceeded() {
        let status = BudgetStatus {
            tenant_id: "tenant-a".into(),
            token_limit: 100,
            tokens_used: 100,
            remaining: 0,
            percentage: 100,
            alert_threshold: 80,
            would_exceed: true,
        };
        assert!(status.would_exceed);
        assert!(!status.would_exceed_with(0));
        assert!(status.would_exceed_with(1));
    }

    #[test]
    fn validate_token_budget_inputs_reject_bad_values() {
        assert!(validate_token_limit(0).is_ok());
        assert!(validate_token_limit(-1).is_err());
        assert!(validate_period("daily").is_ok());
        assert!(validate_period("weekly").is_ok());
        assert!(validate_period("monthly").is_ok());
        assert!(validate_period("yearly").is_err());
    }

    #[test]
    fn compute_period_bounds_produces_valid_ranges() {
        let now = Utc::now();

        let (s, e) = compute_period_bounds("daily", now);
        assert!(e > s);
        assert_eq!(e - s, 86_400);

        let (s, e) = compute_period_bounds("weekly", now);
        assert!(e > s);
        assert_eq!(e - s, 7 * 86_400);

        let (s, e) = compute_period_bounds("monthly", now);
        assert!(e > s);
        // Exact duration varies by month; just assert forward movement.
        let start_dt = chrono::DateTime::from_timestamp(s, 0).unwrap();
        let end_dt = chrono::DateTime::from_timestamp(e, 0).unwrap();
        assert!(end_dt > start_dt);
        assert_eq!(start_dt.day(), 1);
        assert_eq!(end_dt.day(), 1);
    }
}
