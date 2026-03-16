# Chat & Conversations

Send messages to ARES agents and manage multi-turn conversations.

---

## Send a message

```
POST /api/chat
```

Send a message to an agent and receive a response. ARES routes the message to the appropriate agent based on the `agent_type` parameter, or uses the default router agent if none is specified.

### Authentication

Requires a JWT access token: `Authorization: Bearer <jwt_access_token>`

### Request body

| Parameter    | Type   | Required | Description                                                                 |
|-------------|--------|----------|-----------------------------------------------------------------------------|
| `message`    | string | Yes      | The user's message or prompt.                                               |
| `agent_type` | string | No       | Which agent handles the request (e.g., `"product"`, `"research"`, `"router"`). Defaults to the router agent. |
| `context_id` | string | No       | Conversation context ID. Pass this value back on subsequent requests to continue a multi-turn conversation. |

### Response

```json
{
  "response": "Here's what I found about your question...",
  "agent": "product",
  "context_id": "ctx_a1b2c3d4",
  "sources": null
}
```

| Field        | Type        | Description                                                        |
|-------------|-------------|--------------------------------------------------------------------|
| `response`   | string      | The agent's response text.                                         |
| `agent`      | string      | The agent that handled the request.                                |
| `context_id` | string      | Context identifier. Pass this back to continue the conversation.   |
| `sources`    | array\|null | Source references, if the agent performed retrieval. Otherwise `null`. |

### Examples

#### curl

```bash
curl -X POST https://api.ares.dirmacs.com/api/chat \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -d '{
    "message": "What pricing plans do you offer?",
    "agent_type": "product"
  }'
```

#### Python

```python
import requests

response = requests.post(
    "https://api.ares.dirmacs.com/api/chat",
    headers={
        "Content-Type": "application/json",
        "Authorization": "Bearer eyJhbGciOi..."
    },
    json={
        "message": "What pricing plans do you offer?",
        "agent_type": "product"
    }
)

data = response.json()
print(data["response"])

# Continue the conversation using the returned context_id
follow_up = requests.post(
    "https://api.ares.dirmacs.com/api/chat",
    headers={
        "Content-Type": "application/json",
        "Authorization": "Bearer eyJhbGciOi..."
    },
    json={
        "message": "How does the Pro plan compare to Enterprise?",
        "context_id": data["context_id"]
    }
)
```

#### JavaScript

```javascript
const response = await fetch("https://api.ares.dirmacs.com/api/chat", {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
    "Authorization": "Bearer eyJhbGciOi..."
  },
  body: JSON.stringify({
    message: "What pricing plans do you offer?",
    agent_type: "product"
  })
});

const data = await response.json();
console.log(data.response);

// Continue the conversation
const followUp = await fetch("https://api.ares.dirmacs.com/api/chat", {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
    "Authorization": "Bearer eyJhbGciOi..."
  },
  body: JSON.stringify({
    message: "How does the Pro plan compare to Enterprise?",
    context_id: data.context_id
  })
});
```

---

## Stream a response

```
POST /api/chat/stream
```

Send a message and receive the response as a stream of Server-Sent Events (SSE). Each event contains a text chunk. This is the recommended approach for user-facing applications where you want to display the response as it is generated.

The request body is identical to `POST /api/chat`.

### Authentication

Requires a JWT access token: `Authorization: Bearer <jwt_access_token>`

### Response format

The response uses the `text/event-stream` content type. Each SSE event contains a chunk of the agent's response:

```
data: Here's
data:  what I
data:  found about
data:  your question...
```

Collect all chunks to form the complete response. The connection closes automatically when the response is complete.

### Examples

#### curl

```bash
curl -N -X POST https://api.ares.dirmacs.com/api/chat/stream \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -H "Accept: text/event-stream" \
  -d '{
    "message": "Explain quantum computing",
    "agent_type": "research"
  }'
```

#### Python

```python
import requests

response = requests.post(
    "https://api.ares.dirmacs.com/api/chat/stream",
    headers={
        "Content-Type": "application/json",
        "Authorization": "Bearer eyJhbGciOi...",
        "Accept": "text/event-stream"
    },
    json={
        "message": "Explain quantum computing",
        "agent_type": "research"
    },
    stream=True
)

for line in response.iter_lines():
    if line:
        decoded = line.decode("utf-8")
        if decoded.startswith("data: "):
            chunk = decoded[6:]  # Strip "data: " prefix
            print(chunk, end="", flush=True)
```

#### JavaScript

```javascript
const response = await fetch("https://api.ares.dirmacs.com/api/chat/stream", {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
    "Authorization": "Bearer eyJhbGciOi...",
    "Accept": "text/event-stream"
  },
  body: JSON.stringify({
    message: "Explain quantum computing",
    agent_type: "research"
  })
});

const reader = response.body.getReader();
const decoder = new TextDecoder();

while (true) {
  const { done, value } = await reader.read();
  if (done) break;

  const text = decoder.decode(value, { stream: true });
  for (const line of text.split("\n")) {
    if (line.startsWith("data: ")) {
      const chunk = line.slice(6);
      process.stdout.write(chunk); // Node.js
      // Or append to DOM in browsers
    }
  }
}
```

---

## Conversations

Manage stored conversations and their message history.

### List conversations

```
GET /api/conversations
```

Returns all conversations for the authenticated user.

**Authentication:** JWT required.

```bash
curl https://api.ares.dirmacs.com/api/conversations \
  -H "Authorization: Bearer eyJhbGciOi..."
```

### Get a conversation

```
GET /api/conversations/{id}
```

Returns a single conversation along with its full message history.

**Authentication:** JWT required.

| Parameter | Type   | In   | Description         |
|-----------|--------|------|---------------------|
| `id`      | string | path | The conversation ID |

```bash
curl https://api.ares.dirmacs.com/api/conversations/conv_abc123 \
  -H "Authorization: Bearer eyJhbGciOi..."
```

### Update a conversation

```
PUT /api/conversations/{id}
```

Update the title of a conversation.

**Authentication:** JWT required.

**Request body:**

```json
{
  "title": "Pricing discussion"
}
```

```bash
curl -X PUT https://api.ares.dirmacs.com/api/conversations/conv_abc123 \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -d '{"title": "Pricing discussion"}'
```

### Delete a conversation

```
DELETE /api/conversations/{id}
```

Permanently delete a conversation and all its messages.

**Authentication:** JWT required.

```bash
curl -X DELETE https://api.ares.dirmacs.com/api/conversations/conv_abc123 \
  -H "Authorization: Bearer eyJhbGciOi..."
```

---

## User memory

```
GET /api/memory
```

Retrieve memory and preferences that ARES has learned from your conversations. This includes user preferences, context, and behavioral patterns the system has observed.

**Authentication:** JWT required.

```bash
curl https://api.ares.dirmacs.com/api/memory \
  -H "Authorization: Bearer eyJhbGciOi..."
```
