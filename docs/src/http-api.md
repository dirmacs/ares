# HTTP API

ARES exposes one Axum HTTP service. The main router mounts every application route under the `/api` prefix (`crates/ares-http/src/lib.rs`). The server also answers `GET /health` outside `/api`. This chapter documents the routes as implemented in `crates/ares-http/src/api/routes.rs`.

## Base URL

The server binds to `server.host` and `server.port` from its configuration file. Defaults are `127.0.0.1` and port `3000` (`crates/ares-http/src/config.rs`). All paths below are relative to `http://localhost:3000/api`.

```bash
curl -s http://localhost:3000/health
```

The `/health` route returns the plain text `OK`. The server binary adds `GET /health/detailed` and `GET /config/info` next to it.

## Authentication

Three schemes exist. Pick the scheme that matches the route group.

### JWT bearer tokens (user routes)

Register or log in to get a token pair:

```bash
curl -s -X POST http://localhost:3000/api/auth/register \
  -H "Content-Type: application/json" \
  -d '{"email": "user@example.com", "password": "secret", "name": "User"}'
```

```json
{
  "access_token": "<jwt>",
  "refresh_token": "<jwt>",
  "expires_in": 3600
}
```

Send the access token as a bearer token:

```bash
Authorization: Bearer <access_token>
```

The middleware also accepts `?token=<access_token>` as a query parameter. EventSource clients cannot set custom headers, so they use this fallback. Tokens are HS256 JSON Web Tokens signed with the `JWT_SECRET` environment variable. A token carries claims `sub`, `email`, `exp`, `iat`, and optionally `jti` and `tenant_id`.

### Admin secret (admin routes)

Admin routes check the `X-Admin-Secret` header against the `ADMIN_API_KEY` environment variable. As an alternative, admin routes accept a JWT with an admin role claim.

```bash
curl -s http://localhost:3000/api/admin/stats \
  -H "X-Admin-Secret: $ADMIN_API_KEY"
```

A rejected request answers `401` with:

```json
{"error":"Admin access requires X-Admin-Secret header or JWT with admin role"}
```

### Tenant API keys (`/v1` routes)

Routes under `/v1` authenticate machine clients with tenant API keys. Keys start with `ares_` and travel in the same bearer header:

```bash
Authorization: Bearer ares_<key>
```

A missing or malformed key answers `401` with messages such as `Missing Authorization header` or `Invalid Authorization format. Expected: Bearer ares_...`.

## Response Envelope

Successful handlers return the documented payload directly. Errors return one consistent shape with two fields, `error` and `code` (`crates/ares-http/src/error.rs`):

```json
{
  "error": "agent my-agent not found",
  "code": "NOT_FOUND"
}
```

`code` values use stable SCREAMING_SNAKE_CASE identifiers. Middleware rejections before a handler runs answer with only the `error` field.

## Chat

### POST /chat

Runs one agent turn. Requires a JWT bearer token.

Request fields (`ChatRequest`, `crates/ares-types/src/types/mod.rs`):

| Field | Type | Notes |
| --- | --- | --- |
| `message` | string | Required. The user message. |
| `agent_type` | string | Optional. Defaults to the router agent. |
| `context_id` | string | Optional. Continues a conversation. |
| `workspace_id` | string | Optional. Eruka workspace scope. |
| `model` | string | Optional per-request model override. |

```bash
curl -s -X POST http://localhost:3000/api/chat \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"message": "Summarize my notes", "agent_type": "researcher"}'
```

Response (`ChatResponse`):

```json
{
  "response": "Here is the summary...",
  "agent": "researcher",
  "context_id": "8f14e45f-ea9b-4d2a-9c3b-1f6a2b7c9d01",
  "sources": [
    {"title": "Meeting notes", "url": null, "relevance_score": 0.87}
  ]
}
```

### POST /chat/stream and GET /chat/stream

Streams Server-Sent Events with the same request body. The `GET` variant reads the fields from query parameters for EventSource clients. Each event is a `StreamEvent` object with fields `event`, `content`, `agent`, `context_id`, and `error`; absent optional fields are omitted:

```
data: {"event":"start","agent":"researcher","context_id":"8f14e45f-..."}

data: {"event":"token","content":"Here "}

data: {"event":"done","agent":"researcher","context_id":"8f14e45f-..."}
```

Event types are `start`, `token`, `done`, and `error`.

### POST /research

Runs deep research. Body is `{"query": "...", "depth": 3, "max_iterations": 10}`; both limits are optional.

### GET /memory

Returns stored facts and preferences for the authenticated user:

```json
{
  "user_id": "42",
  "preferences": [
    {"category": "communication", "key": "style", "value": "concise", "confidence": 0.9}
  ],
  "facts": [
    {
      "id": "f-1", "user_id": "42", "category": "work",
      "fact_key": "timezone", "fact_value": "UTC+1", "confidence": 0.95
    }
  ]
}
```

An empty memory returns no body content.

## Agents

### User agents (JWT)

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/agents` | Public list of shared agents. |
| GET | `/user/agents` | List the caller's agents. |
| POST | `/user/agents` | Create an agent. |
| GET | `/user/agents/{name}` | Read one agent. |
| PUT | `/user/agents/{name}` | Update one agent. |
| DELETE | `/user/agents/{name}` | Delete one agent. |
| POST | `/user/agents/import` | Import an agent from TOON format. |
| GET | `/user/agents/{name}/export` | Export an agent to TOON format. |

Create body (`CreateUserAgentReq`):

```json
{
  "name": "my-agent",
  "display_name": "My Agent",
  "description": "Answers billing questions",
  "model": "gpt-4o-mini",
  "system_prompt": "You are a billing assistant.",
  "tools": ["calculator"],
  "max_tool_iterations": 10,
  "parallel_tools": false,
  "is_public": false,
  "extra": {}
}
```

Responses carry `id`, `usage_count`, `average_rating`, `created_at`, and `updated_at` alongside the input fields.

### Loop-mode agents (JWT)

`POST /loops/start` starts a loop run, `GET /loops` lists loops, `DELETE /loops/{id}` stops one.

### Conversations (JWT)

`GET /conversations` lists conversations. `GET`, `PUT`, and `DELETE` on `/conversations/{id}` read, rename, and delete one.

## Workflows, Skills, Tools

Workflows require a JWT:

- `GET /workflows` lists available workflows.
- `POST /workflows/{workflow_name}` executes one.

With the `skills` feature enabled:

- `GET /skills` lists skills.
- `GET /skills/{name}` reads one skill.

Admin surfaces manage runtime tools and skills with the `X-Admin-Secret` header:

| Method | Path | Purpose |
| --- | --- | --- |
| GET / POST | `/admin/runtime-tools` | List or create tools. |
| GET | `/admin/runtime-tools/capabilities` | List tool capability descriptors. |
| GET / PUT / DELETE | `/admin/runtime-tools/{id}` | Manage one tool. |
| POST | `/admin/runtime-tools/{id}/test` | Execute a tool with sample input. |
| GET | `/admin/runtime-tools/{id}/versions` | List versions. |
| POST | `/admin/runtime-tools/{id}/rollback/{version}` | Roll back. |
| GET / POST | `/admin/skills` | List or create skills. |
| POST | `/admin/skills/run` | Run a skill. |
| GET / PUT / DELETE | `/admin/skills/{id}` | Manage one skill. |

Tool test example. The body field `input_args` holds the JSON arguments passed to the tool's `execute` method:

```bash
curl -s -X POST http://localhost:3000/api/admin/runtime-tools/7/test \
  -H "X-Admin-Secret: $ADMIN_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"input_args": {"x": 2, "y": 3}}'
```

```json
{"ok": true, "output": {"sum": 5}, "error": null, "latency_ms": 4}
```

Skill run example. `tenant_id` must name an existing tenant:

```bash
curl -s -X POST http://localhost:3000/api/admin/skills/run \
  -H "X-Admin-Secret: $ADMIN_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"skill_id": "summarize", "tenant_id": "tenant-a", "input": {"text": "..."}}'
```

## RAG

These routes need the `local-embeddings` and `ares-vector` features at build time. They require a JWT. Collections are scoped per user; the server prefixes your collection name with your user id internally.

### POST /rag/ingest

Body fields come from `RagIngestRequest`: `collection`, `content`, plus optional `title`, `source`, `tags`, and `chunking_strategy`.

```bash
curl -s -X POST http://localhost:3000/api/rag/ingest \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "collection": "notes",
    "content": "Quarterly review text...",
    "title": "Q3 review",
    "tags": ["finance"]
  }'
```

```json
{
  "chunks_created": 4,
  "document_ids": ["d1", "d2", "d3", "d4"],
  "collection": "notes"
}
```

### POST /rag/search

Strategy is one of `semantic`, `bm25`, `fuzzy`, or `hybrid`. Defaults: `limit` 10, `threshold` 0.0, `rerank` false.

```bash
curl -s -X POST http://localhost:3000/api/rag/search \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"collection": "notes", "query": "budget owners", "limit": 5, "strategy": "hybrid"}'
```

```json
{
  "results": [
    {
      "id": "d1",
      "content": "Budget owners meet on Mondays.",
      "score": 0.91,
      "metadata": {}
    }
  ],
  "total": 5,
  "strategy": "hybrid",
  "reranked": false,
  "duration_ms": 23
}
```

### Collection management

- `GET /rag/collections` lists collections as `CollectionInfo` objects.
- `DELETE /rag/collection` deletes one. Body: `{"collection": "notes"}`. Response: `{"success": true, "collection": "notes", "documents_deleted": 12}`.

## MCP

Model Context Protocol surface is read-only today:

```bash
curl -s http://localhost:3000/api/mcp/runtime_tool_capabilities \
  -H "X-Admin-Secret: $ADMIN_API_KEY"
```

The route lives behind the admin middleware because it merges into the admin router set (`build_routes`).

## `/v1` External API

Machine clients use tenant API keys. Metered routes record usage per call:

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/v1/chat` | Chat completion. |
| POST | `/v1/research` | Deep research run. |
| POST | `/v1/agents/{name}/run` | Run a named agent. |
| POST | `/v1/agents/{name}/sandbox-run` | Sandbox execution. |
| GET | `/v1/agents` | List agents visible to the tenant. |
| GET | `/v1/agents/{name}` | Read one agent. |
| GET | `/v1/agents/{name}/runs` | List run history. |
| GET | `/v1/agents/{name}/logs` | List run logs. |
| GET | `/v1/usage` | Tenant usage summary. |
| GET / POST | `/v1/api-keys` | List or create API keys. |
| DELETE | `/v1/api-keys/{id}` | Revoke a key. |
| POST | `/v1/search/semantic` | Semantic search (feature-gated). |
| DELETE | `/v1/tenant/data` | Delete all tenant data. |

Example:

```bash
curl -s -X POST http://localhost:3000/api/v1/chat \
  -H "Authorization: Bearer ares_$API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"message": "Hello"}'
```

Quota breaches answer with a quota-exceeded error body.

## Webhooks, OAuth, Events

Public routes without authentication:

- `POST /webhooks/{trigger_id}` — webhook receiver for triggers.
- `GET /oauth/authorize` and `GET /oauth/callback` — connector OAuth flow.
- `POST /events/document-upload` and `POST /events/field-change` — event ingestion.

## Admin Surfaces

All admin routes take the `X-Admin-Secret` header. Route groups in `routes.rs`:

| Group | Example routes |
| --- | --- |
| Tenants | `POST/GET /admin/tenants`, `GET /admin/tenants/{tenant_id}`, `POST/GET /admin/tenants/{tenant_id}/api-keys`, `GET .../usage`, `PUT .../quota`, `GET .../usage/daily` |
| Provisioning | `POST /admin/provision-client` |
| Tenant agents | `GET/POST /admin/tenants/{tenant_id}/agents`, `PUT/DELETE .../agents/{agent_name}`, `.../versions`, `.../rollback/{version}`, `.../test`, `.../runs`, `.../stats`, `.../feedback/*` |
| Cross-tenant agents | `GET/POST /admin/agents`, `GET/PUT/DELETE /admin/agents/{tenant_id}/{agent_name}`, `.../versions`, `.../rollback/{version}`, `GET/POST /admin/agents/emergency-stop` |
| Templates and models | `GET/POST /admin/agent-templates`, `DELETE /admin/agent-templates/{id}`, `GET /admin/models` |
| Alerts and audit | `GET /admin/alerts`, `POST /admin/alerts/{alert_id}/resolve`, `GET /admin/audit-log` |
| Deployment | `POST /admin/deploy`, `GET /admin/deploy/{deploy_id}`, `GET /admin/deploys`, `GET /admin/services`, `GET /admin/services/{service_name}/logs` |
| Model tiers | `GET/POST /admin/tenants/{tenant_id}/model-tiers`, `GET/PUT/DELETE .../{tier_name}` |
| Allowlists | `GET/POST .../allowed-tools`, `.../allowed-models`, `.../allowed-rag-sources`, each with `DELETE .../{name}` |
| Triggers and pipelines | `GET/POST .../triggers`, `PUT/DELETE .../triggers/{id}`, same shape for `pipelines` and platform-wide `/admin/triggers`, `/admin/pipelines` |
| Fleet providers | `GET /admin/fleet-providers`, `GET .../capabilities`, `PUT/DELETE .../{provider_name}`, `POST .../verify` |
| Schedules | `GET/POST /admin/schedules`, `PUT/DELETE /admin/schedules/{id}`, tenant variants and `GET .../missed-runs` |
| Connectors | `GET/POST /admin/connectors`, `PUT/DELETE /admin/connectors/{id}`, tenant connectors and `oauth-creds` |
| Billing | `GET .../billing/summary`, `GET .../billing/line-items`, `GET /admin/billing/model-rates`, `GET /admin/billing/unit-rates` |
| Budgets | `GET/PUT/DELETE /admin/run-history/budgets/{tenant_id}`, `GET/PUT /admin/token-budgets/{tenant_id}`, `GET .../status`, `POST .../reset`, `GET .../usage`, `GET /admin/run-history/alerts`, `POST /admin/run-history/alerts/{id}/acknowledge` |
| Run history | `GET/POST /admin/run-history/llm-calls`, `GET .../llm-calls/{id}`, same shape for `tool-calls`, `GET /admin/run-history/costs/{run_id}`, `GET .../costs`, `GET/POST .../health-metrics`, `GET .../model-metrics`, `GET /admin/runs/live` (active-run stream) |
| Runtime providers | `GET/POST /admin/runtime_providers`, `GET/DELETE /admin/runtime_providers/{name}` |
| Platform stats | `GET /admin/stats` |

### Cordis Service Lifecycle

These routes manage the plugin runtime. Unknown loader state answers `503`.

Retire removes a service; provide re-registers a known direct service:

```bash
curl -s -X POST http://localhost:3000/api/admin/cordis/services/events_service/provide \
  -H "X-Admin-Secret: $ADMIN_API_KEY"
```

```json
{"provided": true, "service": "events_service", "type": "cordis::events::EventsService"}
```

### POST /admin/cordis/services/{name}/replace

Rolling drain-and-shift replacement of a journaled provider. The body must be `{"config": <value>}` carrying the new configuration. Success answers:

```json
{"replaced": true, "plugin": "calc", "fiber_id": 17}
```

A refusal (unknown plugin label, untracked provider, failing trial) leaves the old provider serving untouched and answers `409 {"replaced": false, "service": "calc", "reason": "..."}`. A missing `config` field answers `400`.

### Cordis Entries

Entries live in a TOML program file. Routes:

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/admin/cordis/entries` | List the entry tree. |
| PUT | `/admin/cordis/entries` | Upsert an entry. |
| PATCH | `/admin/cordis/entries/{id}` | Partial update. |
| DELETE | `/admin/cordis/entries/{id}` | Remove an entry. |
| POST | `/admin/cordis/entries/{id}/toggle` | Enable or disable. |
| POST | `/admin/cordis/entries/reload` | Reload from disk. |
| POST | `/admin/cordis/entries/{id}/move` | Relocate an entry. |
| GET | `/admin/cordis/events` | Per-event dispatch counters. |
| GET | `/admin/cordis/undo` | Pending undo labels per fiber. |

#### PATCH /admin/cordis/entries/{id}

Applies only the present fields: `config`, `disabled`, `isolate`, `intercept`. An empty body is a validated no-op that still persists and re-applies the tree. Present `parent` or `position` fields move the entry first. Invalid moves answer `409`; unknown ids answer `404`.

When the new configuration fails the factory pre-flight, the response carries a structured `issues` array next to the legacy `error` string. Each issue has a `message` and a `path`:

```json
{
  "applied": [],
  "patched": false,
  "reloaded": false,
  "error": "config pre-flight failed for calc: missing url",
  "issues": [{"message": "missing url", "path": ["calc", "url"]}]
}
```

Success answers `200`:

```json
{"applied": [], "patched": true, "entry": {"id": "calc"}}
```

#### POST /admin/cordis/entries/{id}/move

Relocates the entry and its whole `{id}:*` descendant namespace under a new parent. Body fields:

- `parent` — string entry id, or `null` to move to the tree root.
- `position` — non-negative integer child index. Omit it to append after the target's existing children.

```bash
curl -s -X POST http://localhost:3000/api/admin/cordis/entries/calc/move \
  -H "X-Admin-Secret: $ADMIN_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"parent": "tools-group", "position": 0}'
```

Pure structural moves preserve fiber identity; running fibers never restart. Renamed descendant ids appear as old-to-new pairs:

```json
{
  "moved": true,
  "noop": false,
  "renamed": [["calc", "tools-group:calc"]],
  "applied": []
}
```

Invalid moves (unknown ids, moves under one's own descendant, id collisions) answer `409 {"moved": false, "error": "..."}` without touching the file or the live tree. Unknown entry ids answer `404`.
