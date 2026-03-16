# Guide: Build a Chat Agent

This guide walks you through creating a custom chat agent on ARES — from defining its behavior to testing it in production.

---

## What is an Agent?

An ARES agent is a configured LLM endpoint with a specific personality, instructions, and tool access. Each agent has:

- A **name** — unique identifier used in API calls
- A **model** — which LLM powers it (e.g., `llama-3.3-70b`, `claude-3.5-sonnet`)
- A **system prompt** — instructions that define the agent's behavior
- **Tools** — optional capabilities like `calculator` or `web_search`
- **Configuration** — max tokens, temperature, and other parameters

You can create agents in two ways: via the configuration file or via the API.

---

## Option 1: Define in ares.toml

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

Restart ARES to load the new agent. It will be available immediately at `/api/chat` using `agent_type: "financial-analyst"`.

### TOON Config Format

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

The TOON format structures the system prompt into semantic fields that ARES assembles into a coherent prompt. This makes agent behavior easier to reason about and modify.

---

## Option 2: Create via API

For tenant-specific agents or agents you want to manage programmatically, use the API.

### As a Platform Admin

```bash
curl -X POST https://api.ares.dirmacs.com/api/admin/tenants/{tenant_id}/agents \
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

### As an Authenticated User

```bash
curl -X POST https://api.ares.dirmacs.com/api/user/agents \
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

## Testing Your Agent

### Basic Chat

Send a message to your agent:

```bash
curl -X POST https://api.ares.dirmacs.com/api/chat \
  -H "Authorization: Bearer <jwt_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [
      {"role": "user", "content": "What is the compound annual growth rate if revenue went from $1M to $1.8M over 3 years?"}
    ],
    "agent_type": "financial-analyst"
  }'
```

**Expected response:**

```json
{
  "content": "To calculate the Compound Annual Growth Rate (CAGR):\n\nCAGR = (Ending Value / Beginning Value)^(1/n) - 1\nCAGR = ($1,800,000 / $1,000,000)^(1/3) - 1\nCAGR = (1.8)^(0.3333) - 1\nCAGR = 1.2164 - 1\nCAGR = 0.2164\n\n**The CAGR is 21.64%.**\n\nThis means revenue grew at an average annual rate of approximately 21.6% over the 3-year period.",
  "model": "llama-3.3-70b",
  "tokens_used": 287
}
```

### Multi-Turn Conversation

Include the conversation history in the `messages` array:

```bash
curl -X POST https://api.ares.dirmacs.com/api/chat \
  -H "Authorization: Bearer <jwt_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [
      {"role": "user", "content": "What is the CAGR from $1M to $1.8M over 3 years?"},
      {"role": "assistant", "content": "The CAGR is 21.64%..."},
      {"role": "user", "content": "What if the period was 5 years instead?"}
    ],
    "agent_type": "financial-analyst"
  }'
```

### With Tool Usage

If your agent has tools enabled, ARES handles the tool calling loop automatically. You send a normal chat message, and the agent uses tools as needed:

```bash
curl -X POST https://api.ares.dirmacs.com/api/chat \
  -H "Authorization: Bearer <jwt_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [
      {"role": "user", "content": "Calculate 15% annual compound interest on $50,000 over 10 years"}
    ],
    "agent_type": "financial-analyst"
  }'
```

The agent will internally call the calculator tool to compute `50000 * (1.15)^10` and return the formatted result.

### Streaming

For real-time responses, use the streaming endpoint:

```bash
curl -X POST https://api.ares.dirmacs.com/api/chat/stream \
  -H "Authorization: Bearer <jwt_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [
      {"role": "user", "content": "Explain the difference between NPV and IRR"}
    ],
    "agent_type": "financial-analyst"
  }'
```

This returns a Server-Sent Events stream. See the [V1 API docs](../enterprise/v1-api.md) for client-side streaming examples.

---

## Iterating on the System Prompt

The system prompt is the most important part of your agent. Here are practical guidelines:

### Be Specific About Format

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

### Define Boundaries

Tell the agent what it should *not* do:

```
Constraints:
- Never provide specific investment advice or recommend buying/selling securities
- If asked about tax implications, recommend consulting a tax professional
- Do not speculate about future market movements
- If you don't have enough data to answer accurately, say so
```

### Include Examples

For complex formatting requirements, show the agent what you want:

```
When comparing metrics, use this format:

| Metric | 2024 | 2025 | Change |
|--------|------|------|--------|
| Revenue | $1.2M | $1.8M | +50% |
| EBITDA | $300K | $480K | +60% |
```

### Test Edge Cases

After writing your system prompt, test these scenarios:

1. **Off-topic requests** — Does the agent stay in character or helpfully redirect?
2. **Ambiguous inputs** — Does the agent ask for clarification?
3. **Tool failures** — Does the agent handle tool errors gracefully?
4. **Long conversations** — Does the agent maintain context over multiple turns?

---

## Adding Tool Access

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

See the [Tool Calling guide](./tool-calling.md) for details on how tool execution works.

---

## Choosing a Model

Different models have different strengths. Consider these factors when choosing:

| Model | Provider | Best For |
|---|---|---|
| `llama-3.3-70b` | Groq | General-purpose, fast, good reasoning |
| `llama-3.1-8b` | Groq | Simple tasks, lowest latency |
| `deepseek-r1` | NVIDIA | Complex reasoning, chain-of-thought |
| `claude-3.5-sonnet` | Anthropic | Nuanced writing, careful analysis |

Start with `llama-3.3-70b` for most use cases. It offers a strong balance of capability, speed, and cost. Move to a specialized model only if you have a specific need.

Check available models with:

```bash
curl https://api.ares.dirmacs.com/api/admin/models \
  -H "X-Admin-Secret: your-admin-secret"
```
