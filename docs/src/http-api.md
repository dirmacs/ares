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

#### Token anatomy

`Claims` lives in `crates/ares-types/src/types/mod.rs`; the signing logic lives in `crates/ares-http/src/auth/jwt.rs`. One decoded access token:

```json
{
  "sub": "9b2f3c58-4b1e-4a7d-9c11-0f5a6b8c2d10",
  "email": "user@example.com",
  "exp": 1756221600,
  "iat": 1756220700
}
```

| Claim | Presence | Meaning |
| --- | --- | --- |
| `sub` | always | User id. Refresh tokens must match the session row by this field. |
| `email` | always | Account email. |
| `exp`, `iat` | always | Expiry and issue time as Unix seconds. Validation allows a 60-second clock-skew leeway. |
| `jti` | refresh tokens only | Random UUID that identifies one refresh session. Access tokens omit it. |
| `tenant_id` | tenant-scoped tokens only | Tenant that issued or owns the session. |

Defaults from `AuthConfig` (`crates/ares-http/src/config.rs`): access tokens live 900 seconds (15 minutes), refresh tokens 604800 seconds (7 days). `expires_in` in the response echoes the configured access expiry, so read it instead of hard-coding 900.

#### Register, login, refresh, logout

Validation runs before any database call:

| Route | Failure | Response |
| --- | --- | --- |
| `POST /auth/register` | Empty email or password under 8 characters | `400 {"error":"Email required and password must be at least 8 characters"}` |
| `POST /auth/register` | Email already registered | `400 {"error":"User already exists"}` |
| `POST /auth/login` | Unknown email or wrong password | `401 {"error":"Invalid credentials"}` |

Passwords hash with Argon2id; refresh tokens are stored only as SHA-256 hashes.

The refresh flow rotates sessions — each refresh token works exactly once (`refresh_token`, `crates/ares-http/src/api/handlers/auth.rs`):

1. Verify the refresh token's HS256 signature and expiry.
2. Hash it and look up the session row. No row answers `401 {"error":"Refresh token has been revoked or expired"}`.
3. Compare the session's user id with the `sub` claim. A mismatch answers `401 {"error":"Token mismatch"}`.
4. Delete the old session row.
5. Issue and return a fresh pair; the new refresh token lands in its own session.

```bash
curl -s -X POST http://localhost:3000/api/auth/refresh \
  -H "Content-Type: application/json" \
  -d '{"refresh_token": "<refresh_token>"}'
```

The response is a full `TokenResponse`. Reuse of an already-rotated refresh token fails at step 2, which is why clients must persist the newest pair after every call.

`POST /auth/logout` takes `{"refresh_token": "..."}`, deletes the matching session by hash, and returns `{"message":"Logged out successfully"}` even when the session is already gone.

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

#### Scheme matrix

| Property | JWT bearer | Admin secret | Tenant API key |
| --- | --- | --- | --- |
| Credential | Access token from login/register | Static value of `ADMIN_API_KEY` env var | Key created via `POST /v1/api-keys`, prefix `ares_` |
| Header | `Authorization: Bearer <access_token>` or `?token=` | `X-Admin-Secret: <value>`; a JWT with an admin role claim also works | `Authorization: Bearer ares_<key>` |
| Route group | `/chat`, `/research`, `/user/agents`, `/conversations`, `/workflows`, ... | `/admin/*` | `/v1/*` |
| Identity | User id in `sub` claim | None (operator) | Tenant resolved from the key row |
| Metering | No quota gate at the middleware | Not metered | Monthly and daily quota checks run before the handler |
| Revocation | Refresh rotation plus logout deletes the session | Rotate the environment variable and restart | Revoke with `DELETE /v1/api-keys/{id}` |

The middleware rejects malformed `/v1` credentials before touching the database (`crates/ares-http/src/middleware/api_key_auth.rs`). All format failures answer `401 {"error": "<message>"}`:

| Condition | Message |
| --- | --- |
| No `Authorization` header | `Missing Authorization header` |
| Header not valid ASCII | `Invalid Authorization header` |
| Value does not start with `Bearer ` (case-sensitive) | `Invalid Authorization format. Expected: Bearer ares_...` |
| Key does not start with `ares_` | `Invalid API key format. Must start with ares_` |
| Key is well formed but unknown | `Invalid API key` |

Quota breaches answer `429`: `Monthly request quota exceeded` or `Daily rate limit exceeded`. The monthly check wins when both are exhausted. Tier limits come from the tenant's quota row; the unit tests pin examples — a Free-tier tenant blocks at 1,000 requests per month or 50 per day, a Dev-tier tenant at 2,000 per day, Enterprise tiers allow large volumes. Infrastructure faults answer `500` with messages such as `Tenant database not configured`, `Failed to verify API key`, `Failed to check usage`, or `Failed to check rate limit`.

## Response Envelope

Successful handlers return the documented payload directly. Errors return one consistent shape with two fields, `error` and `code` (`crates/ares-http/src/error.rs`):

```json
{
  "error": "agent my-agent not found",
  "code": "NOT_FOUND"
}
```

### Error catalog

Handlers return `HttpError`, which wraps `AppError` (`crates/ares-http/src/error.rs`). The status comes from `AppError::status_code()` and the code from `AppError::code()`, both in `crates/ares-types/src/types/mod.rs`. The mapping is fixed:

| `AppError` variant | HTTP status | `code` | Example message prefix |
| --- | --- | --- | --- |
| `Database` | 500 | `DATABASE_ERROR` | `Database error:` |
| `LLM` | 500 | `LLM_ERROR` | `LLM error:` |
| `Auth` | 401 | `AUTHENTICATION_FAILED` | `Authentication error:` |
| `NotFound` | 404 | `NOT_FOUND` | `Not found:` |
| `InvalidInput` | 400 | `INVALID_INPUT` | `Invalid input:` |
| `Configuration` | 500 | `CONFIGURATION_ERROR` | `Configuration error:` |
| `External` | 502 | `EXTERNAL_SERVICE_ERROR` | `External service error:` |
| `Internal` | 500 | `INTERNAL_ERROR` | `Internal error:` |
| `Unavailable` | 503 | `INTERNAL_ERROR` | `Service unavailable:` |
| `RateLimited` | 429 | `INTERNAL_ERROR` | `Rate limited:` |
| `FeatureDisabled` | 400 | `INTERNAL_ERROR` | `Feature disabled:` |

Three variants carry status codes that do not match their code class: `Unavailable` answers 503 but reports `INTERNAL_ERROR`, and `RateLimited` answers 429 while `FeatureDisabled` answers 400, both also reporting `INTERNAL_ERROR`. Match on the status plus the message prefix, not on `code` alone.

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

#### Event anatomy

`StreamEvent` and its four constructors live in `crates/ares-http/src/api/handlers/chat.rs`. Absent optional fields are omitted from the JSON, never sent as `null`:

| Event | Fields set | Producer behavior |
| --- | --- | --- |
| `start` | `agent` (`"<name> (system)"`), `context_id` | Sent once before any model output, after agent resolution succeeds. |
| `token` | `content` | One per streamed token chunk. No `agent` or `context_id`. |
| `done` | `agent` (`"{AgentType:?} ({source})"`, for example `"Sales (system)"`), `context_id` | Final event of a successful run. |
| `error` | `error`; `context_id` when known | Terminal. Failures before a context exists (admission denial, missing Llm service) omit `context_id` entirely. |

An admission failure yields an error event with no other fields:

```
data: {"event":"error","error":"monthly quota exceeded"}
```

A mid-stream failure carries the conversation scope:

```
data: {"event":"start","agent":"product (system)","context_id":"8f14e45f-..."}

data: {"event":"token","content":"Here "}

data: {"event":"error","context_id":"8f14e45f-...","error":"Stream error: provider closed connection"}
```

The endpoint attaches an SSE keep-alive comment every 15 seconds (`Sse::keep_alive` in `chat_stream_response`). Idle connections therefore never time out silently; clients should ignore comment frames.

The `GET` variant takes the request fields as query parameters (`ChatStreamQuery`): `message` (required), plus optional `agent_type`, `context_id`, and `workspace_id`. Authenticate it with `Authorization: Bearer` or the `?token=` fallback:

```bash
curl -N -s "http://localhost:3000/api/chat/stream?message=Summarize%20my%20notes&agent_type=researcher&token=$ACCESS_TOKEN"
```

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

## Pagination and Filtering

List endpoints use two different parameter conventions.

### `/v1` page-based pagination

`GET /v1/agents`, `GET /v1/agents/{name}/runs`, and `GET /v1/agents/{name}/logs` take `page` and `per_page` query parameters and return a `Paginated<T>` envelope (`crates/ares-http/src/api/handlers/v1/shared.rs`):

```json
{
  "items": [],
  "total": 0,
  "page": 1,
  "per_page": 20,
  "total_pages": 0
}
```

| Parameter | Normalization | Notes |
| --- | --- | --- |
| `page` | Defaults to 1; values under 1 clamp to 1 | |
| `per_page` | Defaults to 20 for agents, 25 for runs; caps at 100 | Logs default to 50 |

Example:

```bash
curl -s "http://localhost:3000/api/v1/agents?page=2&per_page=50" \
  -H "Authorization: Bearer ares_$API_KEY"
```

### Admin limit/offset pagination

Admin list endpoints in `crates/ares-http/src/api/handlers/admin/audit.rs` and siblings take `limit` and `offset`. The handler clamps the values before querying:

| Route group | Parameters | Clamping |
| --- | --- | --- |
| `GET /admin/alerts` | `limit`, `severity`, `resolved` | Default limit 50, cap 200; filter by severity string and resolved flag |
| `GET /admin/audit-log` | `limit`, `offset` | Default limit 50, cap 200 |
| `GET .../tenants/{tenant_id}/usage/daily` | `days` | Default 30, cap 90 |
| Tenant agent runs (`.../agents/{name}/runs`) | `limit`, `offset` | Default 50, cap 200 |
| Feedback summary (`.../{agent_name}/feedback/summary`) | `days` | Default 30, clamped to 1..366 |
| Missed runs (`GET .../schedules/{id}/missed-runs`) | `limit` | Default 10, clamped to 1..100 |
| Run history costs (`POST /admin/run-history/costs`) | `limit`, `offset` in body | Limit clamped to 1..10000 |

Tenant-scoped list routes such as `/admin/triggers`, `/admin/pipelines`, and `/admin/schedules` require `?tenant_id=`. An empty value answers `400 {"error":"tenant_id query param is required"}`.

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

`POST /admin/cordis/services/{name}/retire` answers `200 {"retired": true, ...}` on removal and `200 {"retired": false, ...}` when the service was already absent. Guarded withdrawal refuses the removal while active consumer fibers still rely on the provider; it answers `409 {"retired": false, "reason": "guarded", "consumers": <N>}`. Names that are not direct Cordis services answer `409` as well — wrapper types are not supported today (`crates/ares-http/src/api/handlers/admin/cordis.rs`, `retire_cordis_service`).

Two read-only routes help interpret those outcomes:

- `GET /admin/cordis/services` lists every tracked fiber with `fiber_id`, `state` (the debug form of `FiberState`: `Active`, `Inactive`, `Loading`, `Failed`, `Reloading`, `Unloading`), `error` when the fiber rests in a terminal state with a message, `disposed`, and `pending_undo_count`.
- `GET /admin/cordis/undo` lists the labeled undo closures still pending per fiber, in registration order. Only labeled undos surface; anonymous ones count toward `pending_undo_count` only.

Both answer `503 {"error":"RegistryService is not provided on this context"}` on library deployments without a registry.

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

When the new configuration fails the factory pre-flight, the response carries a structured `issues` array next to the legacy `error` string. Each issue has a `message` and a `path`. The `error` string is the loader's marker plus the rendered error (`"config pre-flight failed: {error}"`, `crates/cordis/src/loader.rs`; a validation issue renders as `- <message> (at <path>)`):

```json
{
  "applied": [],
  "patched": false,
  "reloaded": false,
  "error": "config pre-flight failed: invalid config: - missing url (at calc.url)",
  "issues": [{"message": "missing url", "path": ["calc", "url"]}]
}
```

#### Move-then-update in one call

A body with a present `parent` or `position` field relocates the entry first, then applies the remaining fields. One request can rename a subtree and reconfigure its root:

```bash
curl -s -X PATCH http://localhost:3000/api/admin/cordis/entries/calc \
  -H "X-Admin-Secret: $ADMIN_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"parent": "tools-group", "config": {"precision": 2}}'
```

The response carries the post-patch entry plus `renamed` old-to-new id pairs from the move phase:

```json
{
  "applied": [],
  "patched": true,
  "renamed": [["calc", "tools-group:calc"]],
  "entry": {"id": "tools-group:calc"}
}
```

The live fiber keeps its identity across the structural move. The journal re-keys the record to the new id while preserving the fiber id, so consumers never observe a restart. The config update then lands under the new id. This sequence comes from the test `patch_endpoint_moves_entry` in `crates/ares-http/src/api/handlers/admin/cordis.rs`.

A position-only body reorders within the current parent; an explicit `"parent": null` moves to the tree root.

#### Failed pre-flight: the issues array

When the patched config fails the factory trial pre-flight, the loader stashes machine-readable issues for the entry and the handler attaches them to the failure body shown above. The status is `422` (`patch_endpoint_returns_structured_issues_on_bad_config`).

The failed trial leaves nothing behind: a follow-up well-formed patch succeeds with `200` and no `issues` field, instead of tripping stale issues from the earlier attempt.

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

Parent semantics in detail (`move_cordis_entry` and `EntryTree::move_entry`):

- `"parent": "<id>"` moves under that entry. Every descendant id prefixed `{moved-id}:` renames mechanically; ids without the prefix stay untouched.
- `"parent": null` moves the entry to the tree root and strips any parent prefix from it and its descendants.
- Omitting the `parent` field behaves like `null`: the entry moves to the tree root. To reorder within the current parent without relocating, use `PATCH` with a `position` only.
- `"position"` must be a non-negative integer. A wrong type answers `400 {"error":"\"position\" must be a non-negative integer"}`. A non-string, non-null `parent` answers `400 {"error":"\"parent\" must be a string or null"}`.

Pure structural moves never restart fibers: the loader detects that plugins, configs, disabled flags, and isolates are identical on both sides, takes the noop path, re-keys journal records while keeping fiber ids, and reports `"noop": true` when nothing but placement changed.
