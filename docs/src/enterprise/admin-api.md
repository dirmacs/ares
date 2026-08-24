# Admin API

ARES operators use the Admin API for full platform management. Use the API to provision tenants, manage agents, monitor usage, and operate the platform.

**Base URL:** `http://localhost:3000`

## Authentication

Every request to `/api/admin/*` must include the admin secret:

```
X-Admin-Secret: <secret>
```

You set this secret in your `ares.toml` configuration file. Guard it carefully because it grants full platform access.

---

## Tenants

### Create tenant

```
POST /api/admin/tenants
```

**Request Body:**

```json
{
  "name": "acme-corp",
  "tier": "pro"
}
```

Valid tiers are `free`, `dev`, `pro`, and `enterprise`.

**Response:**

```json
{
  "id": "tenant-uuid",
  "name": "acme-corp",
  "tier": "pro",
  "created_at": "2026-03-13T00:00:00Z"
}
```

### List tenants

```
GET /api/admin/tenants
```

**Response:**

```json
{
  "tenants": [
    {
      "id": "tenant-uuid",
      "name": "acme-corp",
      "tier": "pro",
      "agent_count": 4,
      "created_at": "2026-03-13T00:00:00Z"
    }
  ]
}
```

### Get tenant details

```
GET /api/admin/tenants/{id}
```

**Response:**

```json
{
  "id": "tenant-uuid",
  "name": "acme-corp",
  "tier": "pro",
  "agent_count": 4,
  "api_key_count": 2,
  "total_runs": 12849,
  "total_tokens": 7291034,
  "created_at": "2026-03-13T00:00:00Z"
}
```

### Update tenant tier

```
PUT /api/admin/tenants/{id}/quota
```

**Request Body:**

```json
{
  "tier": "enterprise"
}
```

**Response:** The updated tenant object.

---

## Provisioning

### Provision a client

```
POST /api/admin/provision-client
```

This is the recommended way to onboard a new enterprise client. The endpoint creates a tenant, clones the correct agent templates, and generates an API key in one database transaction. If one step fails, ARES rolls back everything.

**Request Body:**

```json
{
  "name": "acme-corp",
  "tier": "pro",
  "product_type": "trading",
  "api_key_name": "production"
}
```

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | Yes | Unique tenant name with lowercase letters, numbers, and hyphens |
| `tier` | string | Yes | One of: `free`, `dev`, `pro`, `enterprise` |
| `product_type` | string | Yes | Template set to clone. The shipped set contains `generic`; additional templates are site-configured |
| `api_key_name` | string | Yes | Label for the initial API key |

**Response:**

```json
{
  "tenant_id": "tenant-uuid",
  "tenant_name": "acme-corp",
  "tier": "pro",
  "product_type": "trading",
  "api_key_id": "key-uuid",
  "api_key_prefix": "ares_a1b2",
  "raw_api_key": "ares_a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5",
  "agents_created": [
    "trade-classifier",
    "trade-risk",
    "trade-monitor",
    "trade-reporter"
  ]
}
```

> **Important:** The response contains `raw_api_key` only once. Store it securely. Deliver it to the client through a secure channel.

**curl Example:**

```bash
curl -X POST http://localhost:3000/api/admin/provision-client \
  -H "X-Admin-Secret: your-admin-secret" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "acme-corp",
    "tier": "pro",
    "product_type": "trading",
    "api_key_name": "production"
  }'
```

---

## API Keys

### Create API key for tenant

```
POST /api/admin/tenants/{id}/api-keys
```

**Request Body:**

```json
{
  "name": "staging-key"
}
```

**Response:**

```json
{
  "id": "key-uuid",
  "prefix": "ares_x7k9",
  "raw_key": "ares_x7k9m2p4q8r1s5t3...",
  "created_at": "2026-03-13T00:00:00Z"
}
```

### List API keys for tenant

```
GET /api/admin/tenants/{id}/api-keys
```

**Response:**

```json
{
  "keys": [
    {
      "id": "key-uuid",
      "name": "production",
      "prefix": "ares_a1b2",
      "created_at": "2026-03-13T00:00:00Z",
      "last_used": "2026-03-13T14:00:00Z"
    }
  ]
}
```

---

## Tenant agents

### List tenant agents

```
GET /api/admin/tenants/{id}/agents
```

**Response:**

```json
{
  "agents": [
    {
      "id": "agent-uuid",
      "name": "trade-classifier",
      "agent_type": "classifier",
      "status": "active",
      "model": "llama-3.3-70b",
      "total_runs": 2841,
      "success_rate": 0.991
    }
  ]
}
```

### Create tenant agent

```
POST /api/admin/tenants/{id}/agents
```

**Request Body:**

```json
{
  "name": "custom-analyzer",
  "agent_type": "analyzer",
  "config": {
    "model": "llama-3.3-70b",
    "system_prompt": "You are a financial data analyzer...",
    "tools": ["calculator"],
    "max_tokens": 4096
  }
}
```

### Update tenant agent

```
PUT /api/admin/tenants/{id}/agents/{name}
```

**Request Body:** Same structure as create. ARES updates the fields that you provide.

### Delete tenant agent

```
DELETE /api/admin/tenants/{id}/agents/{name}
```

The endpoint returns `204 No Content` on success.

---

## Templates and models

### List agent templates

```
GET /api/admin/agent-templates?product_type=trading
```

The response lists the pre-configured agent templates for a product type. Provisioning clones these templates.

**Response:**

```json
{
  "templates": [
    {
      "name": "trade-classifier",
      "agent_type": "classifier",
      "product_type": "trading",
      "config": {
        "model": "llama-3.3-70b",
        "system_prompt": "You are a transaction classifier...",
        "tools": []
      }
    }
  ]
}
```

### List available models

```
GET /api/admin/models
```

The response lists all models that are configured across all providers.

**Response:**

```json
{
  "models": [
    {
      "id": "llama-3.3-70b",
      "provider": "groq",
      "context_length": 131072,
      "supports_tools": true
    },
    {
      "id": "deepseek-r1",
      "provider": "nvidia-deepseek",
      "context_length": 65536,
      "supports_tools": false
    },
    {
      "id": "claude-3.5-sonnet",
      "provider": "anthropic",
      "context_length": 200000,
      "supports_tools": true
    }
  ]
}
```

---

## Usage and analytics

### Tenant usage summary

```
GET /api/admin/tenants/{id}/usage
```

**Response:**

```json
{
  "tenant_id": "tenant-uuid",
  "tenant_name": "acme-corp",
  "tier": "pro",
  "period_start": "2026-03-01T00:00:00Z",
  "period_end": "2026-03-31T23:59:59Z",
  "total_runs": 4821,
  "total_tokens": 2847193,
  "quota_runs": 100000,
  "quota_tokens": 10000000
}
```

### Daily usage breakdown

```
GET /api/admin/tenants/{id}/usage/daily?days=30
```

**Response:**

```json
{
  "daily": [
    { "date": "2026-03-13", "runs": 312, "tokens": 184920 },
    { "date": "2026-03-12", "runs": 287, "tokens": 171003 }
  ]
}
```

### Agent run history

```
GET /api/admin/tenants/{id}/agents/{name}/runs?limit=50
```

**Response:**

```json
{
  "runs": [
    {
      "id": "run-uuid",
      "status": "completed",
      "started_at": "2026-03-13T14:22:00Z",
      "duration_ms": 1243,
      "tokens_used": 847
    }
  ]
}
```

### Agent stats

```
GET /api/admin/tenants/{id}/agents/{name}/stats
```

**Response:**

```json
{
  "agent_name": "trade-classifier",
  "total_runs": 2841,
  "successful_runs": 2815,
  "failed_runs": 26,
  "success_rate": 0.991,
  "avg_duration_ms": 1102,
  "avg_tokens": 723,
  "last_run": "2026-03-13T14:22:00Z"
}
```

### Cross-Tenant agent list

```
GET /api/admin/agents
```

The response lists agents across all tenants. This view helps with platform-wide visibility.

### Platform stats

```
GET /api/admin/stats
```

**Response:**

```json
{
  "total_tenants": 12,
  "total_agents": 47,
  "total_runs_today": 3291,
  "total_tokens_today": 1948271,
  "active_alerts": 2
}
```

---

## Alerts and audit

### List alerts

```
GET /api/admin/alerts?severity=critical&resolved=false&limit=100
```

**Query Parameters:**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `severity` | string | all | Filter by: `info`, `warning`, `critical` |
| `resolved` | boolean | all | Filter by resolution status |
| `limit` | integer | `100` | Maximum results to return |

**Response:**

```json
{
  "alerts": [
    {
      "id": "alert-uuid",
      "severity": "critical",
      "message": "Tenant acme-corp approaching token quota (92%)",
      "tenant_id": "tenant-uuid",
      "created_at": "2026-03-13T10:00:00Z",
      "resolved": false
    }
  ]
}
```

### Resolve alert

```
POST /api/admin/alerts/{id}/resolve
```

The endpoint returns `200 OK` with the updated alert object.

### Audit log

```
GET /api/admin/audit-log?limit=50
```

**Response:**

```json
{
  "entries": [
    {
      "id": "entry-uuid",
      "action": "tenant.created",
      "actor": "admin",
      "details": { "tenant_name": "acme-corp", "tier": "pro" },
      "timestamp": "2026-03-13T00:00:00Z"
    },
    {
      "id": "entry-uuid",
      "action": "agent.deleted",
      "actor": "admin",
      "details": { "tenant_id": "...", "agent_name": "old-agent" },
      "timestamp": "2026-03-12T23:00:00Z"
    }
  ]
}
```
