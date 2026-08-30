# ARES, build & deploy instructions

*Copy this file to `/opt/ares/CLAUDE.md` on the VPS.*

## Critical rule

ARES is a **GENERIC** multi-tenant AI agent runtime. It contains zero client-specific code: no client routes, no client tables, no client business logic. Client deployments are external tenants. They call `/v1/chat` with their tenant API keys.

## Build

```bash
cargo build --release --no-default-features --features postgres,mcp
```

If cargo runs out of memory, run `CARGO_BUILD_JOBS=1 cargo build --release --no-default-features --features postgres,mcp`.

## After rebuild

```bash
sudo systemctl restart ares
curl -s localhost:3000/health  # make sure that the server is up
```

## Route parameters

Axum 0.8 uses matchit 0.8, which requires the **`{param}`** syntax. Do not use `:param`. That syntax belongs to Axum 0.7 / matchit 0.7 only. A route with `:param` silently fails with a 404.

Before you touch routes, verify the syntax in use:

```bash
grep -rn "param\|:id\|{id}" crates/ares-http/src/api/routes.rs | head -20
```

## Middleware

Use **`.route_layer()`**, not `.layer()`, for route-specific middleware. `.layer()` also wraps the fallback and leaks the middleware to unmatched routes.

## Configuration

`/opt/ares/ares.toml` is a symlink to `/opt/ares-config/ares.toml`.

To update the configuration, run `cd /opt/ares-config && git pull && sudo systemctl restart ares`.

## Database

```bash
sudo -u postgres psql -d ares
\dt  # list tables
SELECT count(*) FROM usage_events;  # check metering data
```

The `dirmacs` user owns the tables. If you see permission errors, check the ownership with `\dt`.
