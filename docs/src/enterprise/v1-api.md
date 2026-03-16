# V1 Client API

The V1 API is the primary interface for enterprise clients integrating ARES into their applications. All endpoints are scoped to the authenticated tenant — you only see your own agents, runs, and usage.

**Base URL:** `https://api.ares.dirmacs.com`

## Authentication

Every request to `/v1/*` must include your API key in the `Authorization` header:

```
Authorization: Bearer ares_xxx
```

API keys are issued during tenant provisioning. You can create additional keys via the API or request them from your platform administrator.

---

## Agents

### List Agents

```
GET /v1/agents?page=1&per_page=20
```

Returns a paginated list of agents configured for your tenant.

**Query Parameters:**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `page` | integer | `1` | Page number |
| `per_page` | integer | `20` | Results per page |

**Response:**

```json
{
  "agents": [
    {
      "id": "uuid",
      "name": "risk-analyzer",
      "agent_type": "classifier",
      "status": "active",
      "config": { "model": "llama-3.3-70b", "tools": ["calculator"] },
      "created_at": "2026-03-01T00:00:00Z",
      "last_run": "2026-03-13T14:22:00Z",
      "total_runs": 1547,
      "success_rate": 0.982
    }
  ],
  "total": 4,
  "page": 1,
  "per_page": 20
}
```

### Get Agent Details

```
GET /v1/agents/{name}
```

Returns full details for a single agent.

**Response:**

```json
{
  "id": "uuid",
  "name": "risk-analyzer",
  "agent_type": "classifier",
  "status": "active",
  "config": {
    "model": "llama-3.3-70b",
    "system_prompt": "You are a risk analysis agent...",
    "tools": ["calculator"],
    "max_tokens": 2048
  },
  "created_at": "2026-03-01T00:00:00Z",
  "last_run": "2026-03-13T14:22:00Z",
  "total_runs": 1547,
  "success_rate": 0.982
}
```

### Run an Agent

```
POST /v1/agents/{name}/run
```

Execute an agent with the provided input. This is the core endpoint for triggering agent work.

**Request Body:**

```json
{
  "input": {
    "message": "Analyze the risk profile for transaction TX-9921",
    "context": {
      "amount": 15000,
      "currency": "USD",
      "merchant_category": "electronics"
    }
  }
}
```

**Response:**

```json
{
  "id": "run-uuid",
  "agent_id": "agent-uuid",
  "status": "completed",
  "input": { "message": "Analyze the risk profile..." },
  "output": {
    "risk_score": 0.73,
    "risk_level": "medium",
    "reasoning": "Elevated amount for merchant category..."
  },
  "error": null,
  "started_at": "2026-03-13T14:22:00Z",
  "finished_at": "2026-03-13T14:22:01Z",
  "duration_ms": 1243,
  "tokens_used": 847
}
```

If the agent fails, `status` will be `"failed"` and `error` will contain a description.

### List Agent Runs

```
GET /v1/agents/{name}/runs?page=1&per_page=20
```

Returns the run history for a specific agent, newest first.

---

## Chat

### Send a Chat Message

```
POST /v1/chat
```

Send a message to a model or agent and receive a complete response.

**Request Body:**

```json
{
  "messages": [
    { "role": "user", "content": "Summarize Q1 revenue trends." }
  ],
  "model": "llama-3.3-70b",
  "agent_type": "analyst"
}
```

**Response:**

```json
{
  "id": "msg-uuid",
  "content": "Based on the data, Q1 revenue showed...",
  "model": "llama-3.3-70b",
  "tokens_used": 312,
  "finish_reason": "stop"
}
```

### Stream a Chat Response

```
POST /v1/chat/stream
```

Same request body as `/v1/chat`, but returns a Server-Sent Events (SSE) stream.

```
data: {"delta": "Based on", "finish_reason": null}
data: {"delta": " the data,", "finish_reason": null}
data: {"delta": " Q1 revenue", "finish_reason": null}
...
data: {"delta": "", "finish_reason": "stop", "tokens_used": 312}
```

---

## Usage

### Get Usage Summary

```
GET /v1/usage
```

Returns your tenant's usage for the current billing period.

**Response:**

```json
{
  "period_start": "2026-03-01T00:00:00Z",
  "period_end": "2026-03-31T23:59:59Z",
  "total_runs": 4821,
  "total_tokens": 2847193,
  "total_api_calls": 5290,
  "quota_runs": 100000,
  "quota_tokens": 10000000,
  "daily_usage": [
    { "date": "2026-03-13", "runs": 312, "tokens": 184920, "api_calls": 340 },
    { "date": "2026-03-12", "runs": 287, "tokens": 171003, "api_calls": 315 }
  ]
}
```

---

## API Keys

### List API Keys

```
GET /v1/api-keys
```

Returns all API keys for your tenant. The full key secret is never returned after creation.

**Response:**

```json
{
  "keys": [
    {
      "id": "key-uuid",
      "name": "android-production",
      "prefix": "ares_a1b2",
      "created_at": "2026-03-01T00:00:00Z",
      "expires_at": "2027-03-01T00:00:00Z",
      "last_used": "2026-03-13T14:00:00Z"
    }
  ]
}
```

### Create API Key

```
POST /v1/api-keys
```

**Request Body:**

```json
{
  "name": "mobile-app-key",
  "expires_in_days": 365
}
```

`expires_in_days` is optional. If omitted, the key does not expire.

**Response:**

```json
{
  "key": "key-uuid",
  "secret": "ares_x7k9m2p4q8r1s5t3..."
}
```

> **Important:** The `secret` field is only returned once at creation time. Store it securely — it cannot be retrieved again.

### Revoke API Key

```
DELETE /v1/api-keys/{id}
```

Immediately invalidates the key. Returns `204 No Content` on success.

---

## Examples

### Run an Agent (curl)

```bash
curl -X POST https://api.ares.dirmacs.com/v1/agents/risk-analyzer/run \
  -H "Authorization: Bearer ares_x7k9m2p4q8r1s5t3" \
  -H "Content-Type: application/json" \
  -d '{
    "input": {
      "message": "Evaluate this transaction",
      "context": {"amount": 15000, "currency": "USD"}
    }
  }'
```

### Run an Agent (Python)

```python
import requests

API_KEY = "ares_x7k9m2p4q8r1s5t3"
BASE_URL = "https://api.ares.dirmacs.com"

headers = {
    "Authorization": f"Bearer {API_KEY}",
    "Content-Type": "application/json",
}

# Run an agent
response = requests.post(
    f"{BASE_URL}/v1/agents/risk-analyzer/run",
    headers=headers,
    json={
        "input": {
            "message": "Evaluate this transaction",
            "context": {"amount": 15000, "currency": "USD"},
        }
    },
)

result = response.json()
print(f"Status: {result['status']}")
print(f"Output: {result['output']}")
print(f"Duration: {result['duration_ms']}ms")
print(f"Tokens: {result['tokens_used']}")
```

### Check Usage (curl)

```bash
curl https://api.ares.dirmacs.com/v1/usage \
  -H "Authorization: Bearer ares_x7k9m2p4q8r1s5t3"
```

### Check Usage (Python)

```python
response = requests.get(f"{BASE_URL}/v1/usage", headers=headers)
usage = response.json()

print(f"Runs this month: {usage['total_runs']} / {usage['quota_runs']}")
print(f"Tokens this month: {usage['total_tokens']} / {usage['quota_tokens']}")
```

### Chat with Streaming (Python)

```python
import requests
import json

response = requests.post(
    f"{BASE_URL}/v1/chat/stream",
    headers=headers,
    json={
        "messages": [{"role": "user", "content": "Explain quantum computing."}],
        "model": "llama-3.3-70b",
    },
    stream=True,
)

for line in response.iter_lines():
    if line:
        text = line.decode("utf-8")
        if text.startswith("data: "):
            data = json.loads(text[6:])
            print(data.get("delta", ""), end="", flush=True)
```

### Chat with Streaming (JavaScript)

```javascript
const response = await fetch("https://api.ares.dirmacs.com/v1/chat/stream", {
  method: "POST",
  headers: {
    "Authorization": "Bearer ares_x7k9m2p4q8r1s5t3",
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    messages: [{ role: "user", content: "Explain quantum computing." }],
    model: "llama-3.3-70b",
  }),
});

const reader = response.body.getReader();
const decoder = new TextDecoder();

while (true) {
  const { done, value } = await reader.read();
  if (done) break;

  const text = decoder.decode(value);
  for (const line of text.split("\n")) {
    if (line.startsWith("data: ")) {
      const data = JSON.parse(line.slice(6));
      process.stdout.write(data.delta || "");
    }
  }
}
```
