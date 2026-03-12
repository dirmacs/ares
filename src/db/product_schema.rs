use sqlx::PgPool;
use crate::types::{AppError, Result};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProductType {
    Generic,
    Kasino,
    Ehb,
}

impl ProductType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "generic" => Some(Self::Generic),
            "kasino" => Some(Self::Kasino),
            "ehb" => Some(Self::Ehb),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Kasino => "kasino",
            Self::Ehb => "ehb",
        }
    }
}

/// Ensures all tables for a product type exist. Idempotent — safe to call multiple times.
pub async fn ensure_product_schema(pool: &PgPool, product_type: &ProductType) -> Result<()> {
    match product_type {
        ProductType::Generic => Ok(()),
        ProductType::Kasino => ensure_kasino_schema(pool).await,
        ProductType::Ehb => ensure_ehb_schema(pool).await,
    }
}

async fn ensure_kasino_schema(pool: &PgPool) -> Result<()> {
    sqlx::query(r#"
        CREATE TABLE IF NOT EXISTS kasno_devices (
            id TEXT PRIMARY KEY,
            tenant_id TEXT NOT NULL,
            name TEXT NOT NULL,
            device_token TEXT,
            status TEXT NOT NULL DEFAULT 'active',
            block_mode TEXT NOT NULL DEFAULT 'aggressive',
            last_seen BIGINT,
            created_at BIGINT NOT NULL,
            updated_at BIGINT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_kasno_devices_tenant ON kasno_devices(tenant_id);

        CREATE TABLE IF NOT EXISTS kasno_events (
            id TEXT PRIMARY KEY,
            tenant_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            event_type TEXT NOT NULL,
            severity TEXT NOT NULL,
            source TEXT NOT NULL,
            domain TEXT,
            app_package TEXT,
            content TEXT,
            gambling_score REAL,
            action_taken TEXT,
            metadata JSONB,
            created_at BIGINT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_kasno_events_tenant ON kasno_events(tenant_id);
        CREATE INDEX IF NOT EXISTS idx_kasno_events_device ON kasno_events(device_id);
        CREATE INDEX IF NOT EXISTS idx_kasno_events_created ON kasno_events(created_at);
        CREATE INDEX IF NOT EXISTS idx_kasno_events_severity ON kasno_events(severity);

        CREATE TABLE IF NOT EXISTS kasno_risk_scores (
            id TEXT PRIMARY KEY,
            tenant_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            score_date TEXT NOT NULL,
            risk_score REAL NOT NULL,
            factors JSONB,
            trend TEXT,
            summary TEXT,
            computed_at BIGINT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_kasno_risk_unique ON kasno_risk_scores(device_id, score_date);
        CREATE INDEX IF NOT EXISTS idx_kasno_risk_tenant ON kasno_risk_scores(tenant_id);

        CREATE TABLE IF NOT EXISTS kasno_rules (
            id TEXT PRIMARY KEY,
            tenant_id TEXT NOT NULL,
            rule_type TEXT NOT NULL,
            pattern TEXT NOT NULL,
            action TEXT NOT NULL DEFAULT 'block',
            source TEXT NOT NULL DEFAULT 'admin',
            enabled BOOLEAN NOT NULL DEFAULT true,
            hits BIGINT NOT NULL DEFAULT 0,
            created_at BIGINT NOT NULL,
            updated_at BIGINT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_kasno_rules_tenant ON kasno_rules(tenant_id);
    "#)
    .execute(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;
    Ok(())
}

async fn ensure_ehb_schema(pool: &PgPool) -> Result<()> {
    sqlx::query(r#"
        CREATE TABLE IF NOT EXISTS ehb_patients (
            id TEXT PRIMARY KEY,
            tenant_id TEXT NOT NULL,
            name TEXT,
            age INTEGER,
            conditions JSONB,
            medications JSONB,
            created_at BIGINT NOT NULL,
            updated_at BIGINT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_ehb_patients_tenant ON ehb_patients(tenant_id);

        CREATE TABLE IF NOT EXISTS ehb_sessions (
            id TEXT PRIMARY KEY,
            tenant_id TEXT NOT NULL,
            patient_id TEXT NOT NULL,
            session_type TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'active',
            summary TEXT,
            risk_flags JSONB,
            created_at BIGINT NOT NULL,
            updated_at BIGINT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_ehb_sessions_tenant ON ehb_sessions(tenant_id);
        CREATE INDEX IF NOT EXISTS idx_ehb_sessions_patient ON ehb_sessions(patient_id);
    "#)
    .execute(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;
    Ok(())
}
