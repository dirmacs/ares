# Agents

ARES agents are autonomous units that process requests. Each agent uses one LLM model, one system prompt, and one set of tools. Each agent specializes in one domain or task: routing, research, product knowledge, risk analysis, and more.

Agents are defined by four properties:

- **Model**, the LLM that powers the agent (for example, `llama-3.3-70b`, `claude-3-5-sonnet`, `deepseek-r1`).
- **System prompt**, instructions that shape the behavior, personality, and domain knowledge of the agent.
- **Tools**, capabilities that the agent can invoke during processing (for example, `calculator`, `web_search`, `code_interpreter`).
- **Name**, a unique identifier that routes requests to this agent.

An agent is either platform-provided (available to all users) or user-defined (private to the creator). Users create user agents via the API or via TOON configuration.

---

## List all agents

```
GET /api/agents
```

This endpoint returns all available agents on the platform. It does not require authentication.

### Response

```json
[
  {
    "name": "router",
    "description": "Routes incoming requests to the most appropriate specialist agent.",
    "model": "llama-3.3-70b-versatile",
    "tools": []
  },
  {
    "name": "research",
    "description": "Conducts deep multi-step research with source synthesis.",
    "model": "deepseek-r1-distill-llama-70b",
    "tools": ["web_search", "calculator"]
  },
  {
    "name": "product",
    "description": "Answers product-related questions with detailed knowledge.",
    "model": "llama-3.3-70b-versatile",
    "tools": []
  }
]
```

### Examples

#### curl

```bash
curl http://localhost:3000/api/agents
```

#### Python

```python
import requests

response = requests.get("http://localhost:3000/api/agents")
agents = response.json()

for agent in agents:
    print(f"{agent['name']}: {agent['description']}")
```

#### JavaScript

```javascript
const response = await fetch("http://localhost:3000/api/agents");
const agents = await response.json();

agents.forEach(agent => {
  console.log(`${agent.name}: ${agent.description}`);
});
```

---

## User agents

These endpoints create and manage custom agents. User agents are private to your account. You configure each user agent with any available model, a custom system prompt, and a tool selection.

All user agent endpoints require JWT authentication with this header: `Authorization: Bearer <jwt_access_token>`.

### List your agents

```
GET /api/user/agents
```

This endpoint returns all custom agents owned by the authenticated user.

```bash
curl http://localhost:3000/api/user/agents \
  -H "Authorization: Bearer eyJhbGciOi..."
```

### Create an agent

```
POST /api/user/agents
```

This endpoint creates a new custom agent.

#### Request body

| Parameter | Type | Required | Description |
|---------------|----------|----------|----------------------------------------------|
| `name` | string | Yes | Unique agent name (alphanumeric, hyphens). |
| `model` | string | Yes | LLM model identifier. |
| `system_prompt` | string | Yes | Instructions that define agent behavior. |
| `tools` | string[] | No | List of tool names the agent can use. |

#### Example

```bash
curl -X POST http://localhost:3000/api/user/agents \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -d '{
    "name": "code-reviewer",
    "model": "llama-3.3-70b-versatile",
    "system_prompt": "You are an expert code reviewer. Analyze code for bugs, security issues, and style problems. Be concise and actionable.",
    "tools": ["calculator"]
  }'
```

```python
import requests

requests.post(
    "http://localhost:3000/api/user/agents",
    headers={
        "Content-Type": "application/json",
        "Authorization": "Bearer eyJhbGciOi..."
    },
    json={
        "name": "code-reviewer",
        "model": "llama-3.3-70b-versatile",
        "system_prompt": "You are an expert code reviewer. Analyze code for bugs, security issues, and style problems. Be concise and actionable.",
        "tools": ["calculator"]
    }
)
```

### Get agent details

```
GET /api/user/agents/{name}
```

This endpoint returns the full configuration of one user agent.

| Parameter | Type | In | Description |
|-----------|--------|------|------------------|
| `name` | string | path | The agent's name |

```bash
curl http://localhost:3000/api/user/agents/code-reviewer \
  -H "Authorization: Bearer eyJhbGciOi..."
```

### Update an agent

```
PUT /api/user/agents/{name}
```

This endpoint updates the configuration of an existing agent. You can change the model, the system prompt, and the tools.

```bash
curl -X PUT http://localhost:3000/api/user/agents/code-reviewer \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -d '{
    "model": "deepseek-r1-distill-llama-70b",
    "system_prompt": "You are a senior code reviewer specializing in Rust and TypeScript.",
    "tools": ["calculator", "web_search"]
  }'
```

### Delete an agent

```
DELETE /api/user/agents/{name}
```

This endpoint permanently deletes a user agent.

```bash
curl -X DELETE http://localhost:3000/api/user/agents/code-reviewer \
  -H "Authorization: Bearer eyJhbGciOi..."
```

---

## TOON import/export

TOON is the ARES agent configuration format. You import and export agent configurations as TOON files to share agent definitions, back up configurations, or move agents between environments.

### Import a TOON config

```
POST /api/user/agents/import
```

This endpoint imports an agent definition from a TOON configuration file.

```bash
curl -X POST http://localhost:3000/api/user/agents/import \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -d @agent-config.toon
```

### Export as TOON

```
GET /api/user/agents/{name}/export
```

This endpoint exports the agent configuration in TOON format. Use it to share agent definitions or to version them next to your codebase.

```bash
curl http://localhost:3000/api/user/agents/code-reviewer/export \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -o code-reviewer.toon
```
