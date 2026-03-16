# Multi-Tenant Architecture

ARES is a multi-tenant platform. Each enterprise client operates within an isolated tenant, with their own agents, API keys, usage quotas, and data boundaries. This page explains the tenancy model and how to provision new clients.

---

## Core Concepts

### Tenants

A tenant is an isolated namespace on the ARES platform. Each tenant has:

- A unique name and ID
- A tier that determines rate limits and quotas
- Its own set of agents (cloned from templates or created manually)
- One or more API keys for authentication
- Independent usage tracking and billing data

Tenants cannot see or interact with each other's resources. A request authenticated with Tenant A's API key will never return Tenant B's agents, runs, or usage data.

### Tiers

Every tenant is assigned a tier that governs their resource limits:

| Tier | Monthly Requests | Monthly Tokens | Daily Rate Limit | Use Case |
|---|---|---|---|---|
| **Free** | 1,000 | 100,000 | 100/day | Evaluation and testing |
| **Dev** | 10,000 | 1,000,000 | 1,000/day | Development and staging |
| **Pro** | 100,000 | 10,000,000 | 10,000/day | Production workloads |
| **Enterprise** | Unlimited | Unlimited | Unlimited | High-volume clients |

Tiers can be changed at any time via the Admin API without disrupting the tenant's service.

### Agent Templates

When a tenant is provisioned, ARES clones a set of pre-configured agent templates based on the specified `product_type`. Templates provide a working starting point that can be customized after creation.

Available product types:

| Product Type | Templates Included | Description |
|---|---|---|
| `generic` | General-purpose agents | Default chat and analysis agents |
| `kasino` | `kasino-classifier`, `kasino-risk`, `kasino-transaction`, `kasino-report` | Transaction analysis and reporting |
| `ehb` | Health-oriented agents | eHealthBuddy clinical agents |

Each template defines the agent's model, system prompt, tool access, and default configuration. After provisioning, agents can be freely modified or new ones added.

### API Key Scoping

Every API key is bound to exactly one tenant. When a request arrives with an API key:

1. ARES looks up the key and identifies the associated tenant
2. All operations execute within that tenant's scope
3. Usage is tracked against that tenant's quotas
4. The response only includes that tenant's data

A tenant can have multiple API keys (e.g., separate keys for production, staging, and mobile). Each key's usage is tracked individually but counts toward the shared tenant quota.

### Data Isolation

Tenant isolation is enforced at the database query level. Every data-accessing query includes the tenant ID as a filter condition. This means:

- Agent listings only return the requesting tenant's agents
- Run history only shows runs from the requesting tenant
- Usage data only reflects the requesting tenant's consumption
- There is no API surface to query across tenant boundaries (except via the Admin API)

---

## Provisioning Flow

The recommended way to onboard a new client is the atomic provisioning endpoint. It creates all required resources in a single database transaction.

### Step 1: Provision the Client

```bash
curl -X POST https://api.ares.dirmacs.com/api/admin/provision-client \
  -H "X-Admin-Secret: your-admin-secret" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "acme-corp",
    "tier": "pro",
    "product_type": "kasino",
    "api_key_name": "production"
  }'
```

**Response:**

```json
{
  "tenant_id": "550e8400-e29b-41d4-a716-446655440000",
  "tenant_name": "acme-corp",
  "tier": "pro",
  "product_type": "kasino",
  "api_key_id": "key-uuid",
  "api_key_prefix": "ares_a1b2",
  "raw_api_key": "ares_a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5",
  "agents_created": [
    "kasino-classifier",
    "kasino-risk",
    "kasino-transaction",
    "kasino-report"
  ]
}
```

This single call:

1. Creates the tenant with the specified tier
2. Looks up the agent templates for the given `product_type`
3. Clones each template as a tenant-specific agent
4. Generates an API key bound to the new tenant
5. Returns the raw API key (shown only once)

If any step fails, the entire operation is rolled back. You will never end up with a half-provisioned tenant.

### Step 2: Deliver the API Key

Securely deliver the `raw_api_key` to your client. This is the only time the full key is visible — ARES stores only a hashed version internally.

### Step 3: Verify the Setup

Confirm the tenant's agents are accessible using their new API key:

```bash
curl https://api.ares.dirmacs.com/v1/agents \
  -H "Authorization: Bearer ares_a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5"
```

The client should see their four provisioned agents.

### Step 4: Test an Agent Run

```bash
curl -X POST https://api.ares.dirmacs.com/v1/agents/kasino-classifier/run \
  -H "Authorization: Bearer ares_a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5" \
  -H "Content-Type: application/json" \
  -d '{
    "input": {
      "message": "Classify this transaction: $500 at electronics store"
    }
  }'
```

---

## Managing Tenants After Provisioning

### Add More Agents

```bash
curl -X POST https://api.ares.dirmacs.com/api/admin/tenants/{tenant_id}/agents \
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

### Issue Additional API Keys

```bash
curl -X POST https://api.ares.dirmacs.com/api/admin/tenants/{tenant_id}/api-keys \
  -H "X-Admin-Secret: your-admin-secret" \
  -H "Content-Type: application/json" \
  -d '{"name": "staging-key"}'
```

### Upgrade a Tenant's Tier

```bash
curl -X PUT https://api.ares.dirmacs.com/api/admin/tenants/{tenant_id}/quota \
  -H "X-Admin-Secret: your-admin-secret" \
  -H "Content-Type: application/json" \
  -d '{"tier": "enterprise"}'
```

### Monitor Usage

```bash
# Current period summary
curl https://api.ares.dirmacs.com/api/admin/tenants/{tenant_id}/usage \
  -H "X-Admin-Secret: your-admin-secret"

# Daily breakdown for the last 30 days
curl "https://api.ares.dirmacs.com/api/admin/tenants/{tenant_id}/usage/daily?days=30" \
  -H "X-Admin-Secret: your-admin-secret"
```

---

## Architecture Notes

- **Shared infrastructure:** All tenants run on the same ARES instance and database. Isolation is logical, not physical. This keeps operational costs low for the MVP phase.
- **Atomic provisioning:** The provisioning endpoint uses a database transaction. If agent template cloning fails halfway through, the tenant and any partially created resources are rolled back.
- **Key hashing:** API keys are hashed before storage. The raw key is returned exactly once during creation. Lost keys must be revoked and replaced.
- **Auto-migration:** ARES runs database migrations on startup (`sqlx::migrate!()`). New tenant-related schema changes are applied automatically when the server restarts.
