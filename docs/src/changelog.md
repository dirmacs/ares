# Changelog

All notable changes to ARES are documented here. This project follows [Semantic Versioning](https://semver.org/).

---

## 0.6.3

**Multi-provider LLM, tenant agents, and enterprise metering.**

This release transforms ARES from a single-provider system into a full multi-provider LLM platform with enterprise-grade tenant management.

### Added

- **Multi-provider LLM routing** — Support for 4 providers (Groq, Anthropic, NVIDIA DeepSeek, Ollama) and 11 models through a unified API.
- **Model tier system** — `fast`, `balanced`, `powerful`, `deepseek`, and `local` tiers with automatic provider routing.
- **Tenant agent system** — Agents stored in the database per tenant. Template-based provisioning with full CRUD via admin API.
- **Agent templates** — Seed templates applied automatically on startup. New tenants receive a default agent set.
- **Usage metering** — `usage_events` table, `monthly_usage_cache`, and `daily_rate_limits` for tracking tokens, requests, and costs per tenant.
- **API key authentication** — `Authorization: Bearer ares_xxx` on `/v1/*` routes with tenant scoping.
- **Kasino enterprise agents** — 4 specialized agent templates (`kasino-classifier`, `kasino-risk`, `kasino-transaction`, `kasino-report`) for the first enterprise client.
- **Kasino API routes** — Both JWT-protected (`/api/kasino/*`) and API-key (`/v1/kasino/*`) endpoints.
- **Admin provisioning API** — Atomic tenant creation: schema + agents + API key in a single operation.

### Changed

- Chat handler now resolves `tenant_id` from authentication context instead of hardcoded values.
- Provider configuration moved from code to `ares.toml` for runtime flexibility.
- Rate limit enforcement now operates at both the provider and tenant level.

### Fixed

- Chat handler tenant_id resolution for multi-tenant requests.

---

## 0.6.2

**Streaming and SSE support.**

### Added

- **Server-Sent Events streaming** — `POST /v1/chat/stream` endpoint for real-time, token-by-token responses.
- **Stream handler** — Unified streaming across all providers with consistent SSE format.
- **Context continuation** — `context_id` parameter for maintaining conversation history across requests.

### Changed

- Response format standardized to `{"response", "agent", "context_id"}` across all endpoints.

---

## 0.6.1

**Tool calling and RAG foundations.**

### Added

- **Tool calling framework** — Define tools per agent. ARES manages the tool-call loop, execution, and response assembly.
- **RAG pipeline** — Retrieval-augmented generation with pluggable document stores.
- **Workflow engine** — Chain multiple agents into multi-step workflows with deterministic execution.

### Changed

- Agent configuration schema extended to support tool definitions and RAG settings.

---

## 0.5.0

**JWT authentication and user management.**

### Added

- **User registration and login** — `POST /api/auth/register`, `POST /api/auth/login`.
- **JWT token lifecycle** — 15-minute access tokens, refresh token rotation, logout/invalidation.
- **Role-based access** — User roles with permission checks on protected routes.
- **Admin authentication** — `X-Admin-Secret` header for internal administration endpoints.

### Changed

- All `/api/*` routes now require JWT authentication.
- Error responses standardized with `error` and `code` fields.

---

## 0.4.0

**PostgreSQL backend and multi-tenant schema.**

### Added

- **PostgreSQL integration** — Full migration from in-memory storage to PostgreSQL with `sqlx`.
- **Auto-migration** — `sqlx::migrate!()` runs on startup. No manual SQL required.
- **Tenant schema** — `tenants`, `tenant_agents`, and `api_keys` tables with foreign key relationships.
- **Tenant tiers** — Free, Dev, Pro, and Enterprise tiers with configurable limits.

### Changed

- All state persistence moved from in-memory structures to PostgreSQL.
- Connection pooling via `sqlx::PgPool` with configurable pool size.

---

For the complete commit history, see the [ARES repository on GitHub](https://github.com/dirmacs/ares).
