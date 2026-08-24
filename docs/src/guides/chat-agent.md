# Guide: build a chat agent

This guide shows how to create a custom chat agent on ARES, from the behavior definition to the production test.

---

## What is an agent?

An ARES agent is a configured LLM endpoint with a specific personality, instructions, and tool access. Each agent has:

- A **name**, a unique identifier for API calls
- A **model**, the LLM that powers it (e.g., `llama-3.3-70b`, `claude-3.5-sonnet`)
- A **system prompt**, instructions that define the behavior of the agent
- **Tools**, optional capabilities like `calculator` or `web_search`
- A **configuration**, max tokens, temperature, and other parameters

You can create agents in two ways: via the configuration file or via the API.

---

## Option 1: define in ares.toml

For agents that are part of your core platform, define them in the `ares.toml` configuration file:

```toml
[[agents]]
name = "financial-analyst"
model = "llama-3.3-70b"
system_prompt = """
You are a senior financial analyst. You help users understand financial data,
calculate metrics, and provide clear explanations of financial concepts.

Guidelines:
- Always show your calculations step by step
- Use the calculator tool for arithmetic to ensure accuracy
- Present numbers with appropriate formatting (commas, decimal places)
- When uncertain, clearly state your assumptions
"""
tools = ["calculator"]
max_tokens = 4096
```

Restart ARES to load the new agent. The agent is then immediately available at `/api/chat` with `agent_type: "financial-analyst"`.

### TOON config format

ARES also supports the TOON configuration format for more structured agent definitions:

```toml
[[agents]]
name = "support-agent"
model = "llama-3.3-70b"

[agents.toon]
role = "Customer Support Specialist"
personality = "Professional, empathetic, solution-oriented"
knowledge = ["product documentation", "pricing plans", "common issues"]
constraints = [
    "Never make up information about products",
    "Escalate billing disputes to human agents",
    "Always confirm the customer's issue before proposing a solution",
]
tools = ["web_search"]
```

The TOON format structures the system prompt into semantic fields. ARES assembles these fields into a coherent prompt. This structure makes the agent behavior easier to reason about and modify.

---

## Option 2: create via API

For tenant-specific agents, or for programmatic management, use the API.

### As a platform admin

```bash
curl -X POST http://localhost:3000/api/admin/tenants/{tenant_id}/agents \
  -H "X-Admin-Secret: your-admin-secret" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "financial-analyst",
    "agent_type": "analyst",
    "config": {
      "model": "llama-3.3-70b",
      "system_prompt": "You are a senior financial analyst...",
      "tools": ["calculator"],
      "max_tokens": 4096
    }
  }'
```

### As an authenticated user

```bash
curl -X POST http://localhost:3000/api/user/agents \
  -H "Authorization: Bearer <jwt_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "my-analyst",
    "agent_type": "analyst",
    "config": {
      "model": "llama-3.3-70b",
      "system_prompt": "You are a senior financial analyst...",
      "tools": ["calculator"],
      "max_tokens": 4096
    }
  }'
```

---

## Testing your agent

### Basic chat

Send a message to your agent:

```bash
curl -X POST http://localhost:3000/api/chat \
  -H "Authorization: Bearer <jwt_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "message": "What is the compound annual growth rate if revenue went from $1M to $1.8M over 3 years?",
    "agent_type": "financial-analyst"
  }'
```

**Expected response:**

```json
{
  "response": "To calculate the Compound Annual Growth Rate (CAGR):\n\nCAGR = (Ending Value / Beginning Value)^(1/n) - 1\nCAGR = ($1,800,000 / $1,000,000)^(1/3) - 1\nCAGR = (1.8)^(0.3333) - 1\nCAGR = 1.2164 - 1\nCAGR = 0.2164\n\n**The CAGR is 21.64%.**\n\nThis means revenue grew at an average annual rate of approximately 21.6% over the 3-year period.",
  "agent": "financial-analyst",
  "context_id": "ctx_abc123"
}
```

### Multi-Turn Conversation

Pass the `context_id` from the previous response to continue the conversation. ARES manages history server-side:

```bash
curl -X POST http://localhost:3000/api/chat \
  -H "Authorization: Bearer <jwt_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "message": "What if the period was 5 years instead?",
    "agent_type": "financial-analyst",
    "context_id": "ctx_abc123"
  }'
```

### With tool usage

If you enable tools on your agent, ARES handles the tool-calling loop automatically. You send a normal chat message, and the agent uses tools as needed:

```bash
curl -X POST http://localhost:3000/api/chat \
  -H "Authorization: Bearer <jwt_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "message": "Calculate 15% annual compound interest on $50,000 over 10 years",
    "agent_type": "financial-analyst"
  }'
```

The agent calls the calculator tool internally to compute `50000 * (1.15)^10` and returns the formatted result.

### Streaming

For real-time responses, use the streaming endpoint:

```bash
curl -X POST http://localhost:3000/api/chat/stream \
  -H "Authorization: Bearer <jwt_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "message": "Explain the difference between NPV and IRR",
    "agent_type": "financial-analyst"
  }'
```

This endpoint returns a Server-Sent Events stream. See the [V1 API docs](../enterprise/v1-api.md) for client-side streaming examples.

---

## Iterating on the system prompt

The system prompt is the most important part of your agent. Use these practical guidelines:

### Be specific about format

Bad:
```
You are a helpful assistant.
```

Good:
```
You are a financial analyst. When presenting calculations:
- Show each step on its own line
- Use the calculator tool for all arithmetic
- Format currency with $ and commas
- Round percentages to 2 decimal places
- End with a bold summary line
```

### Define boundaries

Tell the agent what it must *not* do:

```
Constraints:
- Never provide specific investment advice or recommend buying/selling securities
- If asked about tax implications, recommend consulting a tax professional
- Do not speculate about future market movements
- If you don't have enough data to answer accurately, say so
```

### Include examples

For complex formatting requirements, show the agent an example of the output that you want:

```
When comparing metrics, use this format:

| Metric | 2024 | 2025 | Change |
|--------|------|------|--------|
| Revenue | $1.2M | $1.8M | +50% |
| EBITDA | $300K | $480K | +60% |
```

### Test edge cases

After you write the system prompt, test these scenarios:

1. **Off-topic requests:** Does the agent stay in character or redirect helpfully?
2. **Ambiguous inputs:** Does the agent ask for clarification?
3. **Tool failures:** Does the agent handle tool errors gracefully?
4. **Long conversations:** Does the agent keep context over multiple turns?

---

## Adding tool access

Agents can use built-in tools to extend their capabilities:

```toml
[[agents]]
name = "research-agent"
model = "llama-3.3-70b"
system_prompt = "You are a research agent with access to web search and calculation tools."
tools = ["calculator", "web_search"]
```

Available built-in tools:

| Tool | Description |
|---|---|
| `calculator` | Evaluate mathematical expressions |
| `web_search` | Search the web for current information |

See the [Tool Calling guide](./tool-calling.md) for details on tool execution.

---

## Choosing a model

Different models have different strengths. Consider these factors in your choice:

| Model | Provider | Best For |
|---|---|---|
| `llama-3.3-70b` | Groq | General-purpose, fast, good reasoning |
| `llama-3.1-8b` | Groq | Simple tasks, lowest latency |
| `deepseek-r1` | NVIDIA | Complex reasoning, chain-of-thought |
| `claude-3.5-sonnet` | Anthropic | Nuanced writing, careful analysis |

Start with `llama-3.3-70b` for most use cases. It gives a strong balance of capability, speed, and cost. Move to a specialized model only for a specific need.

Check the available models with:

```bash
curl http://localhost:3000/api/admin/models \
  -H "X-Admin-Secret: your-admin-secret"
```
