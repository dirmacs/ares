# Streaming

ARES streams responses in real time with Server-Sent Events (SSE). You do not wait for the full response. You receive text chunks while the system produces them. This lets user interfaces display text while the system generates it.

---

## Endpoint

```
POST /api/chat/stream
```

This endpoint requires JWT authentication with this header: `Authorization: Bearer <jwt_access_token>`.

```
POST /v1/chat/stream
```

This endpoint requires API key authentication with this header: `Authorization: Bearer ares_xxx`.

Both endpoints accept the request body of [`POST /api/chat`](./chat.md) and return the same SSE format.

---

## SSE format

The response uses the `Content-Type: text/event-stream` content type. Each event contains a `data:` field with one text chunk:

```
data: The
data:  answer
data:  to your
data:  question is
data:  as follows...
```

Each `data:` line is one chunk of the response. Concatenate all chunks to get the complete response. The server closes the connection when it finishes generation.

---

## Examples

### curl

The `-N` flag disables output buffering, so chunks appear immediately:

```bash
curl -N -X POST http://localhost:3000/api/chat/stream \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -H "Accept: text/event-stream" \
  -d '{
    "message": "Explain how neural networks learn",
    "agent_type": "research"
  }'
```

### Python

This example uses the `requests` library with `stream=True`:

```python
import requests

response = requests.post(
    "http://localhost:3000/api/chat/stream",
    headers={
        "Content-Type": "application/json",
        "Authorization": "Bearer eyJhbGciOi...",
        "Accept": "text/event-stream"
    },
    json={
        "message": "Explain how neural networks learn",
        "agent_type": "research"
    },
    stream=True
)

full_response = []

for line in response.iter_lines():
    if line:
        decoded = line.decode("utf-8")
        if decoded.startswith("data: "):
            chunk = decoded[6:]
            print(chunk, end="", flush=True)
            full_response.append(chunk)

complete_text = "".join(full_response)
```

For production use, use `httpx` with async streaming:

```python
import httpx
import asyncio

async def stream_chat(message: str, token: str) -> str:
    chunks = []

    async with httpx.AsyncClient() as client:
        async with client.stream(
            "POST",
            "http://localhost:3000/api/chat/stream",
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {token}",
                "Accept": "text/event-stream"
            },
            json={"message": message}
        ) as response:
            async for line in response.aiter_lines():
                if line.startswith("data: "):
                    chunk = line[6:]
                    print(chunk, end="", flush=True)
                    chunks.append(chunk)

    return "".join(chunks)

result = asyncio.run(stream_chat("Explain how neural networks learn", "eyJhbGciOi..."))
```

### JavaScript (Browser)

This example uses the Fetch API with `ReadableStream`:

```javascript
async function streamChat(message, token) {
  const response = await fetch("http://localhost:3000/api/chat/stream", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${token}`,
      "Accept": "text/event-stream"
    },
    body: JSON.stringify({
      message: message,
      agent_type: "research"
    })
  });

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let fullResponse = "";

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;

    const text = decoder.decode(value, { stream: true });
    for (const line of text.split("\n")) {
      if (line.startsWith("data: ")) {
        const chunk = line.slice(6);
        fullResponse += chunk;

        // Update your UI here
        document.getElementById("output").textContent = fullResponse;
      }
    }
  }

  return fullResponse;
}
```

### JavaScript (Node.js)

```javascript
async function streamChat(message, token) {
  const response = await fetch("http://localhost:3000/api/chat/stream", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${token}`,
      "Accept": "text/event-stream"
    },
    body: JSON.stringify({ message })
  });

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let fullResponse = "";

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;

    const text = decoder.decode(value, { stream: true });
    for (const line of text.split("\n")) {
      if (line.startsWith("data: ")) {
        const chunk = line.slice(6);
        fullResponse += chunk;
        process.stdout.write(chunk);
      }
    }
  }

  return fullResponse;
}
```

### Go

```go
package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
)

func streamChat(message, token string) (string, error) {
	body, _ := json.Marshal(map[string]string{
		"message":    message,
		"agent_type": "research",
	})

	req, err := http.NewRequest("POST",
		"http://localhost:3000/api/chat/stream",
		bytes.NewReader(body))
	if err != nil {
		return "", err
	}

	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Accept", "text/event-stream")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()

	var fullResponse strings.Builder
	scanner := bufio.NewScanner(resp.Body)

	for scanner.Scan() {
		line := scanner.Text()
		if strings.HasPrefix(line, "data: ") {
			chunk := line[6:]
			fmt.Print(chunk)
			fullResponse.WriteString(chunk)
		}
	}

	return fullResponse.String(), scanner.Err()
}

func main() {
	result, err := streamChat("Explain how neural networks learn", "eyJhbGciOi...")
	if err != nil {
		panic(err)
	}
	fmt.Printf("\n\nFull response length: %d characters\n", len(result))
}
```

---

## Error handling

If the request is invalid or authentication fails, the server returns a standard HTTP error response and not an SSE stream. Check the response status before you read the stream:

```python
response = requests.post(url, headers=headers, json=body, stream=True)

if response.status_code != 200:
    print(f"Error {response.status_code}: {response.text}")
else:
    for line in response.iter_lines():
        # process SSE events
```

```javascript
const response = await fetch(url, { method: "POST", headers, body });

if (!response.ok) {
  throw new Error(`Error ${response.status}: ${await response.text()}`);
}

// proceed with stream reading
```

---

## Best practices

- **Set `Accept: text/event-stream`** in each request. This header signals that you expect a streaming response.
- **Disable client-side buffering** where possible (for example, `-N` in curl, `stream=True` in Python requests).
- **Handle connection drops.** The stream can close unexpectedly because of network problems. Implement retry logic in production applications.
- **Set reasonable timeouts.** A long research query can stream for more than 30 seconds. Configure the timeout of your HTTP client accordingly.
- **Concatenate chunks before use.** One chunk can split a word in half. Process only the complete response downstream.
