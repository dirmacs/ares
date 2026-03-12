-- Migration 002: Kasino Schema — DEPRECATED, DO NOT RUN ON NEW INSTALLS
--
-- ARCHITECTURE NOTE: Client domain data does NOT belong in ARES.
-- ARES is a generic AI agent runtime. Kasino's device/event data
-- belongs in the kasino portal's own backend.
--
-- These tables exist on the production VPS from an earlier mistake.
-- They are harmless but unused. New installs should skip this file.
-- Future: drop these tables with a 005_drop_kasino.sql migration.
--
-- Run: psql -U postgres -d ares -f migrations/002_kasino.sql

-- Devices registered under a kasno tenant
CREATE TABLE IF NOT EXISTS kasno_devices (
    id              TEXT PRIMARY KEY,
    tenant_id       TEXT NOT NULL,
    name            TEXT NOT NULL,
    device_token    TEXT,
    status          TEXT NOT NULL DEFAULT 'active',
    block_mode      TEXT NOT NULL DEFAULT 'aggressive',
    last_seen       BIGINT,
    created_at      BIGINT NOT NULL,
    updated_at      BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_kasno_devices_tenant ON kasno_devices(tenant_id);

-- All events from devices (blocked domains, transactions, risk triggers)
CREATE TABLE IF NOT EXISTS kasno_events (
    id              TEXT PRIMARY KEY,
    tenant_id       TEXT NOT NULL,
    device_id       TEXT NOT NULL,
    event_type      TEXT NOT NULL,
    severity        TEXT NOT NULL,
    source          TEXT NOT NULL,
    domain          TEXT,
    app_package     TEXT,
    content         TEXT,
    gambling_score  REAL,
    action_taken    TEXT,
    metadata        JSONB,
    created_at      BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_kasno_events_tenant ON kasno_events(tenant_id);
CREATE INDEX IF NOT EXISTS idx_kasno_events_device ON kasno_events(device_id);
CREATE INDEX IF NOT EXISTS idx_kasno_events_created ON kasno_events(created_at);
CREATE INDEX IF NOT EXISTS idx_kasno_events_severity ON kasno_events(severity);

-- Daily risk scores per device (computed by kasno-risk agent)
CREATE TABLE IF NOT EXISTS kasno_risk_scores (
    id              TEXT PRIMARY KEY,
    tenant_id       TEXT NOT NULL,
    device_id       TEXT NOT NULL,
    score_date      TEXT NOT NULL,
    risk_score      REAL NOT NULL,
    factors         JSONB,
    trend           TEXT,
    summary         TEXT,
    computed_at     BIGINT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_kasno_risk_unique ON kasno_risk_scores(device_id, score_date);
CREATE INDEX IF NOT EXISTS idx_kasno_risk_tenant ON kasno_risk_scores(tenant_id);

-- Blocking rules (per tenant, synced to devices)
CREATE TABLE IF NOT EXISTS kasno_rules (
    id              TEXT PRIMARY KEY,
    tenant_id       TEXT NOT NULL,
    rule_type       TEXT NOT NULL,
    pattern         TEXT NOT NULL,
    action          TEXT NOT NULL DEFAULT 'block',
    source          TEXT NOT NULL DEFAULT 'admin',
    enabled         BOOLEAN NOT NULL DEFAULT true,
    hits            BIGINT NOT NULL DEFAULT 0,
    created_at      BIGINT NOT NULL,
    updated_at      BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_kasno_rules_tenant ON kasno_rules(tenant_id);
