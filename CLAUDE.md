# ARES — Build & Deploy Instructions

*Copy this file to `/opt/ares/CLAUDE.md` on the VPS.*

## Critical Rule

ARES is a **GENERIC** multi-tenant AI agent runtime. It has ZERO client-specific code. No client routes, no client tables, no client business logic. Kasino and eHB are CLIENTS that call `/v1/chat` with their tenant API keys.

## Build

```bash
cargo build --release --no-default-features --features openai,postgres,mcp
```

If cargo runs out of memory: `CARGO_BUILD_JOBS=1 cargo build --release --no-default-features --features openai,postgres,mcp`

## After Rebuild

```bash
sudo systemctl restart ares
curl -s localhost:3000/health  # verify it's up
```

## Route Parameters

Axum 0.7 uses matchit 0.7 which requires **`:param`** syntax. Do NOT use `{param}` — that's Axum 0.8 / matchit 0.8 only. Using `{param}` silently fails (404).

Verify before touching routes:
```bash
grep -rn "param\|:id\|{id}" src/api/routes.rs | head -20
```

## Middleware

Use **`.route_layer()`** not `.layer()` for route-specific middleware. `.layer()` wraps the fallback too, leaking middleware to unmatched routes.

## Config

`/opt/ares/ares.toml` is a **symlink** → `/opt/ares-config/ares.toml`

To update config: `cd /opt/ares-config && git pull && sudo systemctl restart ares`

## Database

```bash
sudo -u postgres psql -d ares
\dt  # list tables
SELECT count(*) FROM usage_events;  # check metering data
```

Tables owned by `dirmacs` user. If permission errors, check ownership with `\dt`.
