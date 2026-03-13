# Agents

ARES agents are autonomous units that process requests using a configured LLM model, a system prompt, and a set of tools. Each agent is specialized for a particular domain or task — routing, research, product knowledge, risk analysis, and more.

Agents are defined by four properties:

- **Model** — The LLM that powers the agent (e.g., `llama-3.3-70b`, `claude-3-5-sonnet`, `deepseek-r1`).
- **System prompt** — Instructions that shape the agent's behavior, personality, and domain knowledge.
- **Tools** — Capabilities the agent can invoke during processing (e.g., `calculator`, `web_search`, `code_interpreter`).
- **Name** — A unique identifier used to route requests to this agent.

Agents can be platform-provided (available to all users) or user-defined (private, created via API or TOON config).

---

## List all agents

```
GET /api/agents
```

Returns all available agents on the platform. This endpoint does not require authentication.

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
curl https://api.ares.dirmacs.com/api/agents
```

#### Python

```python
import requests

response = requests.get("https://api.ares.dirmacs.com/api/agents")
agents = response.json()

for agent in agents:
    print(f"{agent['name']}: {agent['description']}")
```

#### JavaScript

```javascript
const response = await fetch("https://api.ares.dirmacs.com/api/agents");
const agents = await response.json();

agents.forEach(agent => {
  console.log(`${agent.name}: ${agent.description}`);
});
```

---

## User agents

Create and manage your own custom agents. User agents are private to your account and can be configured with any available model, custom system prompts, and tool selections.

All user agent endpoints require JWT authentication: `Authorization: Bearer <jwt_access_token>`

### List your agents

```
GET /api/user/agents
```

Returns all custom agents owned by the authenticated user.

```bash
curl https://api.ares.dirmacs.com/api/user/agents \
  -H "Authorization: Bearer eyJhbGciOi..."
```

### Create an agent

```
POST /api/user/agents
```

Create a new custom agent.

#### Request body

| Parameter      | Type     | Required | Description                                  |
|---------------|----------|----------|----------------------------------------------|
| `name`         | string   | Yes      | Unique agent name (alphanumeric, hyphens).   |
| `model`        | string   | Yes      | LLM model identifier.                       |
| `system_prompt` | string  | Yes      | Instructions that define agent behavior.     |
| `tools`        | string[] | No       | List of tool names the agent can use.        |

#### Example

```bash
curl -X POST https://api.ares.dirmacs.com/api/user/agents \
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
    "https://api.ares.dirmacs.com/api/user/agents",
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

Retrieve the full configuration of a specific user agent.

| Parameter | Type   | In   | Description      |
|-----------|--------|------|------------------|
| `name`    | string | path | The agent's name |

```bash
curl https://api.ares.dirmacs.com/api/user/agents/code-reviewer \
  -H "Authorization: Bearer eyJhbGciOi..."
```

### Update an agent

```
PUT /api/user/agents/{name}
```

Update an existing agent's configuration. You can modify the model, system prompt, or tools.

```bash
curl -X PUT https://api.ares.dirmacs.com/api/user/agents/code-reviewer \
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

Permanently delete a user agent.

```bash
curl -X DELETE https://api.ares.dirmacs.com/api/user/agents/code-reviewer \
  -H "Authorization: Bearer eyJhbGciOi..."
```

---

## TOON import/export

TOON is ARES's agent configuration format. You can import and export agent configs as TOON to share agent definitions, back up configurations, or migrate agents between environments.

### Import a TOON config

```
POST /api/user/agents/import
```

Import an agent definition from a TOON configuration file.

```bash
curl -X POST https://api.ares.dirmacs.com/api/user/agents/import \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -d @agent-config.toon
```

### Export as TOON

```
GET /api/user/agents/{name}/export
```

Export an agent's configuration in TOON format. Useful for sharing agent definitions or version-controlling them alongside your codebase.

```bash
curl https://api.ares.dirmacs.com/api/user/agents/code-reviewer/export \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -o code-reviewer.toon
```
