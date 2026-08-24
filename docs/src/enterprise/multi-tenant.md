# Multi-Tenant Architecture

ARES is a multi-tenant platform. Each enterprise client operates within an isolated tenant with its own agents, API keys, usage quotas, and data boundaries. This page explains the tenancy model and how to provision new clients.

---

## Core concepts

### Tenants

A tenant is an isolated namespace on the ARES platform. Each tenant has:

- A unique name and ID
- A tier that sets its rate limits and quotas
- Its own set of agents, cloned from templates or created manually
- One or more API keys for authentication
- Independent usage tracking and billing data

Tenants cannot see each other's resources. A request with Tenant A's API key never returns the agents, runs, or usage data of Tenant B.

### Tiers

Every tenant gets a tier that governs its resource limits:

| Tier | Monthly Requests | Monthly Tokens | Daily Rate Limit | Use Case |
|---|---|---|---|---|
| **Free** | 1,000 | 100,000 | 100/day | Evaluation and testing |
| **Dev** | 10,000 | 1,000,000 | 1,000/day | Development and staging |
| **Pro** | 100,000 | 10,000,000 | 10,000/day | Production workloads |
| **Enterprise** | Unlimited | Unlimited | Unlimited | High-volume clients |

You can change a tier at any time through the Admin API without disruption to the service of the tenant.

### Agent templates

When you provision a tenant, ARES clones a set of pre-configured agent templates based on the specified `product_type`. Templates give a working starting point. You customize the agents after creation.

Available product types:

| Product Type | Templates Included | Description |
|---|---|---|
| `generic` | General-purpose agents | Default chat and analysis agents |
| `trading` | `trade-classifier`, `trade-risk`, `trade-monitor`, `trade-reporter` | Transaction analysis and reporting |
| `health` | Health-oriented agents | Clinical support agents (site-provided) |

Each template defines the model, system prompt, tool access, and default configuration of an agent. After provisioning, you can modify the agents freely or add new ones.

### API key scoping

The system binds every API key to exactly one tenant. When a request arrives with an API key:

1. ARES looks up the key and identifies the associated tenant
2. ARES executes all operations within the scope of that tenant
3. Usage tracking counts against the quotas of that tenant
4. The response includes only data from that tenant

A tenant can have multiple API keys, for example separate keys for production, staging, and mobile. The platform tracks each key separately. Each key counts toward the shared quota of the tenant.

### Data isolation

The database query level enforces tenant isolation. Every query for data includes the tenant ID as a filter condition:

- Agent listings return only agents of the requesting tenant
- Run history shows only runs from the requesting tenant
- Usage data reflects only consumption by the requesting tenant
- No API surface queries across tenant boundaries, except through the Admin API

---

## Provisioning flow

The atomic provisioning endpoint is the recommended way to onboard a new client. It creates all required resources in a single database transaction.

### Step 1: provision the client

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

**Response:**

```json
{
  "tenant_id": "550e8400-e29b-41d4-a716-446655440000",
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

This single call:

1. Creates the tenant with the specified tier
2. Looks up the agent templates for the given `product_type`
3. Clones each template as a tenant-specific agent
4. Generates an API key bound to the new tenant
5. Returns the raw API key (shown only once)

If one step fails, ARES rolls back the whole call. You never end up with a half-provisioned tenant.

### Step 2: deliver the API key

Securely deliver the `raw_api_key` to your client. The full key is visible only this one time. ARES stores only a hashed version internally.

### Step 3: verify the setup

Make sure that the agents of the tenant are accessible with the new API key:

```bash
curl http://localhost:3000/v1/agents \
  -H "Authorization: Bearer ares_a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5"
```

The client will see their four provisioned agents.

### Step 4: test an agent run

```bash
curl -X POST http://localhost:3000/v1/agents/trade-classifier/run \
  -H "Authorization: Bearer ares_a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5" \
  -H "Content-Type: application/json" \
  -d '{
    "input": {
      "message": "Classify this transaction: $500 at electronics store"
    }
  }'
```

---

## Managing tenants after provisioning

### Add more agents

```bash
curl -X POST http://localhost:3000/api/admin/tenants/{tenant_id}/agents \
  -H "X-Admin-Secret: your-admin-secret" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "custom-summarizer",
    "agent_type": "summarizer",
    "config": {
      "model": "llama-3.3-70b",
      "system_prompt": "You summarize financial reports concisely.",
      "tools": [],
      "max_tokens": 2048
    }
  }'
```

### Issue additional API keys

```bash
curl -X POST http://localhost:3000/api/admin/tenants/{tenant_id}/api-keys \
  -H "X-Admin-Secret: your-admin-secret" \
  -H "Content-Type: application/json" \
  -d '{"name": "staging-key"}'
```

### Upgrade a tenant's tier

```bash
curl -X PUT http://localhost:3000/api/admin/tenants/{tenant_id}/quota \
  -H "X-Admin-Secret: your-admin-secret" \
  -H "Content-Type: application/json" \
  -d '{"tier": "enterprise"}'
```

### Monitor usage

```bash
# Current period summary
curl http://localhost:3000/api/admin/tenants/{tenant_id}/usage \
  -H "X-Admin-Secret: your-admin-secret"

# Daily breakdown for the last 30 days
curl "http://localhost:3000/api/admin/tenants/{tenant_id}/usage/daily?days=30" \
  -H "X-Admin-Secret: your-admin-secret"
```

---

## Architecture notes

- **Shared infrastructure:** All tenants run on the same ARES instance and database. Isolation is logical, not physical. This keeps operational costs low for the MVP phase.
- **Atomic provisioning:** The provisioning endpoint uses a database transaction. If template cloning fails halfway, ARES rolls back the tenant and all partially created resources.
- **Key hashing:** ARES hashes API keys before storage. The raw key is returned exactly once during creation. You must revoke and replace lost keys.
- **Auto-migration:** ARES runs database migrations on startup (`sqlx::migrate!()`). The server applies new schema changes for tenants automatically at each restart.
