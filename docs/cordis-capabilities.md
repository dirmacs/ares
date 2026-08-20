# Capability Preservation Checklist (Phase 0, Step 7)

**Rule:** Every externally observable capability at `e4f3bcc` must survive the rewrite (or have intentional behavior change documented in Phase 7 step 24). This checklist groups by route namespace and background job, per plan step 7. Each row is a concrete input → expected observable output, not just `cargo test` passing.

---

## Public Routes (no auth)

| Path | Method | Capability | Expected Output |
|------|--------|------------|-----------------|
| `/health` | GET | Liveness | `200 {"status":"ok"}` (or `"OK"` text at `/health` simple) |
| `/health/detailed` | GET | Component status | `200` with per-component latency/status (public Detailed since 2026-06-02 fix) |
| `/auth/register` | POST | User registration | `201` + `User` + JWT pair; `409` on duplicate email |
| `/auth/login` | POST | Login | `200` + JWT pair; `401` on bad password (test `login_wrong_password_unauthorized` is `#[ignore]` pre-existing) |
| `/auth/refresh` | POST | Token refresh | `200` + new access token; `401` on expired |
| `/auth/logout` | POST | Logout (if present) | `200` + revocation |
| `/agents` | GET | List public agents (community + system) | `200` array of `AgentSummary` |
| `/webhooks/*` | POST | Webhook ingress (document_upload, trigger_engine) | `200` + trigger dispatch; `401` if `WEBHOOK_SECRET` mismatch (env lock semantics preserved) |
| `/events/*` | SSE/POST | Events fan-out | SSE stream if enabled |
| `/oauth/*` | GET/POST | OAuth credential flows (if enabled) | Redirect / token exchange |

**Verification:** `curl -s localhost:3000/health` → `200`; `curl -s localhost:3000/api/auth/login -d '{"email":"...","password":"..."}'` → JWT.

---

## Protected Routes (JWT `Authorization: Bearer`)

| Path | Method | Capability | Expected Output |
|------|--------|------------|-----------------|
| `/chat` | POST | Agent chat (non-stream) | `200` `ChatResponse` with `usage` populated, tool-call trace |
| `/chat/stream` | GET/POST (SSE) | Streaming chat | `200` `text/event-stream` SSE chunks, `usage` footer |
| `/research` | POST | Deep research (coordinator) | `200` `ResearchResponse` |
| `/memory` | GET/POST | Conversation memory | CRUD for `conversations`, `messages` |
| `/workflows` | POST/GET | Workflow execution (router/orchestrator) | `200` workflow output serialization |
| `/user/agents` | GET/POST | User-owned agents (tenant_agents table) | `200` filtered by `tenant_id` |
| `/loops` | POST/GET | Loop-mode agent lifecycle (`LoopRegistry`) | `201` + lifecycle status |
| `/conversations` | GET/POST/PATCH/DELETE | Conversation CRUD | `200` `ConversationSummary`/`Details` |
| `/skills` | POST/GET | Skill execution (`SkillEngine`) | `200` skill step trace (`tool_call`/`llm_call`/`condition`) |
| `/rag/*` | POST/GET | RAG: ingest, search, delete_collection, list_collections | `200` + `rag_crate:chunking_strategy` preserved |
| `/deploy` | POST | Deploy registry (`DeployRegistry`) | `202` + background script updates registry |

**Verification:** `curl -s localhost:3000/api/chat -H 'Authorization: Bearer <jwt>' -H 'Content-Type: application/json' -d '{"message":"hello"}'` → valid `ChatResponse` with `usage`; `curl -N localhost:3000/api/chat/stream` → SSE.

---

## Admin Routes (`X-Admin-Secret`)

Admin is the largest surface — 190 KB `admin.rs` at `e4f3bcc`. Split by domain in Phase 6, but same paths/auth must survive.

| Domain | Representative Paths | Capability |
|--------|----------------------|------------|
| Tenants | `POST/GET /api/admin/tenants`, `GET /api/admin/tenants/:id` | Tenant CRUD, `TenantTier` mapping |
| API Keys | `POST /api/admin/api-keys`, `DELETE /api/admin/api-keys/:id` | Tenant API key issuance, `argon2` hash store |
| Usage | `GET /api/admin/usage`, `GET /api/admin/daily-usage` | Aggregated `run_history`/`agent_runs` costs |
| Quotas | `GET/PUT /api/admin/quotas` | Per-tenant daily/monthly quotas (`tenant_model_tiers`) |
| Agents / Versions / Rollback | `GET/POST /api/admin/agents`, `GET /api/admin/agents/:id/versions`, `POST /api/admin/agents/:id/rollback`, `POST /api/admin/agents/emergency-stop` | Agent CRUD, version history (`agent_versions`), emergency stop (`AtomicBool`) |
| Templates | `GET/POST /api/admin/templates` | 4 fleet templates (`tenant_agents::seed_default_templates`) |
| Models | `GET /api/admin/models` | 97 models from live NVIDIA catalog (`NvidiaCatalogCache`) |
| Alerts | `GET /api/admin/alerts`, `POST /api/admin/alerts/:id/ack` | `alerts` table Budget alerts |
| Audit Log | `GET /api/admin/audit-log` | Mutation audit trail |
| Agent Runs / Feedback / Stats | `GET /api/admin/agent-runs`, `GET /api/admin/agent-runs/:id/feedback`, `GET /api/admin/stats` | `run_history.rs` 15 endpoints, 6 tables |
| Emergency Stop | `POST /api/admin/agents/emergency-stop` | Global 503 flag |
| Runtime Providers/Tools | `GET/POST /api/admin/runtime/providers`, `GET/POST /api/admin/runtime/tools`, `POST /api/admin/runtime/tools/:id/test`, `GET /api/admin/runtime/tools/:id/versions` | `runtime_providers` (021), `runtime_tools` (015) + hot-reload `reload()` |
| Fleet Secrets | `GET/PUT /api/admin/fleet-secrets` | Encrypted provider configs (`FleetSecrets` + `FleetProviderSecretsStore`) |
| Connectors | `GET/POST /api/admin/connectors` | `skills_and_connectors` (019) — slack/google/linkedin/salesforce/hubspot prebuilt |
| MCP Servers | `GET/POST /api/admin/mcp/servers` | `McpRegistry` clients (rmcp) |
| Billing | `GET /api/admin/billing/*` | Cost aggregation per tenant/period |
| OAuth | `GET/POST /api/admin/oauth/*` | `oauth_credentials` store |
| Schedules / Triggers / Pipelines | `GET/POST /api/admin/schedules`, `GET/POST /api/admin/triggers`, `GET/POST /api/admin/pipelines` | Cron schedules, event triggers, pipelines (020) |
| Allowlists | `GET/PUT /api/admin/allowlists` | `tenant_allowlist` |
| Token Budgets | `GET/PUT /api/admin/token-budgets` | `token_budgets` |
| Model Tiers | `GET/PUT /api/admin/model-tiers` | Per-tenant tier→model mapping (017) |
| Health Metrics | `GET /api/admin/health-metrics` | Hourly aggregation (`health_metrics_job`) |

**Verification:** Admin CRUD: create tenant → create API key → create runtime tool → create agent → trigger via `/v1/chat` with tenant key → verify isolation (tenant A cannot see tenant B's tools). See Phase 7 E2E (step 24) row 4.

---

## v1 Routes (API key `X-API-Key` or `Authorization: Bearer <api_key>`)

| Path | Method | Capability | Expected Output |
|------|--------|------------|-----------------|
| `/v1/chat` | POST | Tenant-scoped chat (v1 tenant agent runtime) | `200` `ChatResponse` via `v1_tenant_agent_runtime_tests` path |
| `/v1/stream` | POST (SSE) | Tenant-scoped streaming | `200` SSE |
| `/v1/agents` | GET | List tenant-available agents (resolver) | `200` filtered by Tier + allowlist + `allowed_tools` |
| `/v1/*` (extensions) | POST/GET | Tenant product extensions via `ares-server` `base_router` extension pattern (client plugins call `/v1/*` APIs, no client code in ARES) | `200` per extension spec |

**Verification:** `curl -s localhost:3000/v1/chat -H 'X-API-Key: <tenant_key>' -d '{"message":"test"}'` → tenant-resolved agent + `ToolService::list` scoped.

---

## Background Jobs & Engines

| Job | Table / Source | Trigger | Observable Proof |
|-----|----------------|---------|------------------|
| **Scheduler** (`src/scheduler.rs` 28.5 KB, 60s tick, catch-up pass) | `agent_schedules` + `missed_runs` | Cron evaluation `next_run_at`, `tokio::spawn` every 60s | Insert row with `next_run_at` in past → wait 70s → assert `agent_runs` row appears |
| **Pipeline Engine** (`src/pipeline_engine.rs`) | `agent_pipelines` | Conditional evaluation after upstream execution | Pipeline target `agent_runs` `origin: scheduled` preserved |
| **Trigger Engine** (`src/trigger_engine.rs`) | webhook / document-upload / field-change | `POST /webhooks/*` or DB trigger | `agent_runs` `origin: trigger` with `trigger_id` not `pipeline_id` |
| **Skill Engine** (`src/skill_engine.rs` 34 KB, depth limiting) | `skills` + `connectors` | Sequential `ToolCall`/`LlmCall`/`SkillCall`/`Condition` | Real tool calls + LLM calls in skill steps (R50-5 wired) |
| **Workflow Engine** (`src/workflows/engine.rs`, router/orchestrator) | TOON workflows dir | `POST /workflows` | Router vs orchestrator branch, fallback handling |
| **Health Metrics Job** (`src/health_metrics_job.rs` hourly) | `health_metrics` | Hourly aggregation | `GET /api/admin/health-metrics` shows hourly rows |
| **Nvidia Catalog Refresh** (`crates/ares-config/src/nvidia_catalog.rs`, `catalog.start_background_refresh()`) | `build.nvidia.com/models` | Periodic refresh (default interval) | `GET /api/admin/models` returns ~97 models |
| **Runtime Tool/ Provider Hot-Reload** (`runtime_registry.rs`, `provider_registry.rs`) | Postgres `runtime_tools` / `runtime_providers` | Was 60s `ArcSwap` poll → Phase 3 epoch `notify` | Mutate `runtime_tools` DB row → without restart `ToolService::list` reflects change (no 60s stale window) |
| **Agent Version Snapshot** (`src/main.rs` startup snapshot + hot-reload `mpsc::unbounded_channel`) | `config/agents/*.toon` | TOON file change + `DynamicConfigManager::start_watching` | Modify `config/agents/test.toon` → `agent_versions` row appears via `hot_reload` task |

---

## Cross-Cutting Invariants (must hold after redesign)

| Invariant | Proof |
|-----------|-------|
| **Multi-tenant isolation** | Two tenants with disjoint `runtime_tools`/`runtime_providers` → `ToolService::list(tenant)` shows only tenant's own; `LlmService` tier mapping isolated; `ApiKeyAuth` middleware enforces `tenant_id` |
| **Hot-reload without restart** | File + DB mutation → epoch-driven `Fiber::refresh` (not 60s poll); verify via `config/entries.json` reconciliation + `ReflectService::notify(TypeId::of::<RuntimeToolService>())` BFS |
| **Streaming** | `GET /api/chat/stream` + `POST /v1/stream` still produce SSE (`text/event-stream`); `async-stream` + `tokio::sync::broadcast` fan-out preserved |
| **Fallback chains** | `ProviderOverride.fallback_providers` retry on retryable errors (R50-2) → coordinator retry observable via `run_history.llm_calls` + cost hooks |
| **Cost/Usage/Token budgets** | `POST /api/chat` populates `usage` header → `track_usage` middleware → `run_history` + `agent_runs` + `token_budgets` enforcement |
| **Per-agent tool assignment** | `AgentConfig.allowed_tools` filter (R50-1) → `TenantToolAllowed` / `TenantModelAllowed` checks |
| **MCP bridge** | `McpRegistry` → `ToolRegistry` bridge still exposes MCP tools as agent-callable; MCP server direct `AgentExecutionService` path is intentional improvement (latency) not regression |
| **Axum route param syntax** | `:param` (matchit 0.7) stays `:id` not `{id}` until Axum 0.8 upgrade — grep `src/api/routes.rs` if upgraded |
| **Config symlink** | `ares.toml` remains symlink to `/opt/ares-config/ares.toml` on VPS; Loader state in `config/entries.json` / `config/cordis-entries.toon` must not conflict |
| **ARES stays generic** | Zero client-specific routes/tables/logic — client needs are plugins in client's repo calling `/v1/*` |

---

## Intentional Behavior Changes (documented, not regressions)

| Change | Justification |
|--------|---------------|
| MCP server calls `AgentExecutionService` directly instead of HTTP `reqwest` loopback | Latency improvement, eliminates loopback failure mode; observable: same `ChatResponse` but faster, no `localhost:3000` hop in traces |
| 60s poll → epoch `notify` (watch channel + Postgres NOTIFY/LISTEN) | Eliminates stale window, reduces DB load; observable: `runtime_tools` change visible immediately, not up to 60s later |
| 17-step `run_server` → 5–8 `plugin` calls | Simplification, same services initialized; observable: startup logs show same component counts |
| `sequential orchestrator` → `JoinSet` parallel | Throughput improvement; sequential semantics preserved via `Dispatch::Serial` where order matters |

---

## Verification Matrix Hook (Phase 7, Step 22)

For each row above, Phase 7 (steps 22–24) runs:

1. `curl -s localhost:3000/health` → `200`
2. `curl -s localhost:3000/api/chat -H 'Authorization: Bearer <jwt>' -d '{"message":"hello"}'` → valid `ChatResponse` with `usage`
3. `curl -N localhost:3000/api/chat/stream` → SSE
4. Admin CRUD isolation chain (create tenant → api key → runtime tool → agent → `/v1/chat`)
5. Scheduler: insert past `next_run_at` → 70s → `agent_runs` row
6. Hot-reload: modify TOON/DB → assert change without restart
7. Multi-tenant isolation: disjoint tools → `ToolService::list` invisibility

Plus `cargo check` matrix (with and without `postgres`, with `full`) and `rust-doctor --scope baseline --base main` gate (score ≥ baseline projected, worst_tier no regress).
