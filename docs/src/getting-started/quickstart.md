# Quickstart

Get from zero to your first ARES API call in under 5 minutes.

## Prerequisites

- An ARES API key (format: `ares_xxx`). Contact your administrator or use the [Dirmacs Admin](https://admin.dirmacs.com) provisioning UI to generate one.

## 1. Make your first chat request

Send a message to an ARES agent using the chat endpoint.

### curl

```bash
curl -X POST https://api.ares.dirmacs.com/v1/chat \
  -H "Authorization: Bearer ares_xxx" \
  -H "Content-Type: application/json" \
  -d '{
    "message": "What can you help me with?",
    "agent_type": "product"
  }'
```

### Python

```python
import requests

response = requests.post(
    "https://api.ares.dirmacs.com/v1/chat",
    headers={
        "Authorization": "Bearer ares_xxx",
        "Content-Type": "application/json",
    },
    json={
        "message": "What can you help me with?",
        "agent_type": "product",
    },
)

data = response.json()
print(data["response"])
```

### JavaScript

```javascript
const response = await fetch("https://api.ares.dirmacs.com/v1/chat", {
  method: "POST",
  headers: {
    "Authorization": "Bearer ares_xxx",
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    message: "What can you help me with?",
    agent_type: "product",
  }),
});

const data = await response.json();
console.log(data.response);
```

### Response

```json
{
  "response": "I can help you with product information, recommendations, and questions...",
  "agent": "product",
  "context_id": "ctx_a1b2c3d4"
}
```

The `context_id` is returned with every response. Pass it back in subsequent requests to maintain conversation context.

## 2. Try streaming

For real-time, token-by-token output, use the streaming endpoint. ARES streams responses using Server-Sent Events (SSE).

### curl

```bash
curl -N -X POST https://api.ares.dirmacs.com/v1/chat/stream \
  -H "Authorization: Bearer ares_xxx" \
  -H "Content-Type: application/json" \
  -d '{
    "message": "Explain how LLM routing works",
    "agent_type": "product"
  }'
```

The `-N` flag disables output buffering so you see tokens as they arrive.

### Python

```python
import requests

response = requests.post(
    "https://api.ares.dirmacs.com/v1/chat/stream",
    headers={
        "Authorization": "Bearer ares_xxx",
        "Content-Type": "application/json",
    },
    json={
        "message": "Explain how LLM routing works",
        "agent_type": "product",
    },
    stream=True,
)

for line in response.iter_lines():
    if line:
        decoded = line.decode("utf-8")
        if decoded.startswith("data: "):
            print(decoded[6:], end="", flush=True)
```

### JavaScript

```javascript
const response = await fetch("https://api.ares.dirmacs.com/v1/chat/stream", {
  method: "POST",
  headers: {
    "Authorization": "Bearer ares_xxx",
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    message: "Explain how LLM routing works",
    agent_type: "product",
  }),
});

const reader = response.body.getReader();
const decoder = new TextDecoder();

while (true) {
  const { done, value } = await reader.read();
  if (done) break;

  const chunk = decoder.decode(value);
  const lines = chunk.split("\n");

  for (const line of lines) {
    if (line.startsWith("data: ")) {
      process.stdout.write(line.slice(6));
    }
  }
}
```

## 3. Continue a conversation

Use the `context_id` from a previous response to maintain conversation history:

```bash
curl -X POST https://api.ares.dirmacs.com/v1/chat \
  -H "Authorization: Bearer ares_xxx" \
  -H "Content-Type: application/json" \
  -d '{
    "message": "Tell me more about that",
    "agent_type": "product",
    "context_id": "ctx_a1b2c3d4"
  }'
```

## Next steps

- **[Authentication](authentication.md)** — Learn about API keys, JWT tokens, and admin authentication.
- **[Models & Providers](models.md)** — Understand which models are available and how to choose the right one.
