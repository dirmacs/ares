# Guide: Tool Calling

ARES supports tool calling (also known as function calling), allowing agents to use external tools during a conversation. When an agent needs to perform a calculation, search the web, or interact with an external system, it requests a tool call. ARES executes the tool and feeds the result back to the agent, which then incorporates it into its response.

---

## How It Works

Tool calling in ARES follows a multi-turn loop managed by the ToolCoordinator:

```
User message
    |
    v
Agent (LLM) generates response
    |
    ├── If response is final text → return to user
    |
    └── If response contains tool_calls →
            |
            v
        ARES executes each tool
            |
            v
        Results sent back to agent
            |
            v
        Agent generates next response (may call more tools or return final text)
```

This loop continues until the agent produces a final text response or the maximum iteration limit is reached. The entire process is transparent to the caller — you send a chat message and receive a complete response.

---

## Built-in Tools

ARES ships with two built-in tools:

### calculator

Evaluates mathematical expressions and returns the result.

**Capabilities:**
- Basic arithmetic: `+`, `-`, `*`, `/`
- Exponents: `^` or `**`
- Parentheses for grouping
- Common functions: `sqrt`, `sin`, `cos`, `log`, `ln`, `abs`
- Constants: `pi`, `e`

**Example tool call from agent:**
```json
{
  "name": "calculator",
  "arguments": {
    "expression": "50000 * (1.15 ^ 10)"
  }
}
```

**Result returned to agent:**
```json
{
  "result": 202278.25
}
```

### web_search

Searches the web and returns relevant results.

**Example tool call from agent:**
```json
{
  "name": "web_search",
  "arguments": {
    "query": "current US federal interest rate 2026"
  }
}
```

**Result returned to agent:**
```json
{
  "results": [
    {
      "title": "Federal Reserve holds rate at 4.25%",
      "url": "https://...",
      "snippet": "The Federal Reserve maintained its benchmark rate..."
    }
  ]
}
```

---

## Configuring Tool Access

### Per-Agent Tool Filtering

Each agent specifies which tools it can use. An agent without tools configured cannot make tool calls, even if the underlying model supports them.

**In ares.toml:**

```toml
[[agents]]
name = "research-assistant"
model = "llama-3.3-70b"
system_prompt = "You are a research assistant with access to web search and calculation tools."
tools = ["calculator", "web_search"]

[[agents]]
name = "math-tutor"
model = "llama-3.3-70b"
system_prompt = "You are a math tutor. Use the calculator to verify your work."
tools = ["calculator"]

[[agents]]
name = "simple-chat"
model = "llama-3.3-70b"
system_prompt = "You are a conversational assistant."
tools = []
```

**Via the API:**

```bash
curl -X POST https://api.ares.dirmacs.com/api/admin/tenants/{id}/agents \
  -H "X-Admin-Secret: your-admin-secret" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "analyst",
    "agent_type": "analyst",
    "config": {
      "model": "llama-3.3-70b",
      "system_prompt": "You are a data analyst.",
      "tools": ["calculator", "web_search"],
      "max_tokens": 4096
    }
  }'
```

---

## ToolCoordinator

The ToolCoordinator is the internal component that manages the tool calling loop. It handles:

- **Multi-turn orchestration** — Sending tool results back to the model and processing follow-up tool calls
- **Parallel execution** — When the model requests multiple tools in a single turn, they execute concurrently
- **Timeout enforcement** — Individual tool calls are bounded by a configurable timeout
- **Iteration limits** — Prevents infinite tool-calling loops

### Configuration

Tool calling behavior is configured at the server level:

| Setting | Default | Description |
|---|---|---|
| `max_iterations` | `10` | Maximum tool-calling rounds before forcing a text response |
| `parallel_execution` | `true` | Execute multiple tool calls concurrently within a single turn |
| `tool_timeout` | `30s` | Maximum time for a single tool execution |

If an agent hits the iteration limit, ARES instructs the model to produce a final response using the information gathered so far.

---

## Provider Compatibility

Tool calling requires model support. Not all providers and models support function calling:

| Provider | Models | Tool Calling |
|---|---|---|
| Groq | llama-3.3-70b, llama-3.1-8b | Supported |
| Anthropic | claude-3.5-sonnet | Supported |
| NVIDIA | deepseek-r1 | Not supported |
| Ollama | Varies by model | Model-dependent |

If you assign tools to an agent using a model that does not support tool calling, the tools will be ignored and the agent will respond with text only.

---

## Example: Conversation with Tool Calls

Here is what happens internally when a user asks a question that requires tool use.

**User sends:**

```bash
curl -X POST https://api.ares.dirmacs.com/v1/chat \
  -H "Authorization: Bearer ares_xxx" \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [
      {"role": "user", "content": "What is the monthly payment on a $400,000 mortgage at 6.5% for 30 years?"}
    ],
    "agent_type": "financial-analyst"
  }'
```

**Internal flow:**

1. ARES sends the message to the LLM with the calculator tool definition
2. The LLM responds with a tool call:
   ```json
   {
     "tool_calls": [{
       "name": "calculator",
       "arguments": {"expression": "(400000 * (0.065/12) * (1 + 0.065/12)^360) / ((1 + 0.065/12)^360 - 1)"}
     }]
   }
   ```
3. ARES executes the calculator and gets `2528.27`
4. ARES sends the result back to the LLM
5. The LLM produces a final text response incorporating the calculated value

**User receives:**

```json
{
  "content": "The monthly payment on a $400,000 mortgage at 6.5% APR over 30 years would be **$2,528.27**.\n\nThis is calculated using the standard amortization formula...",
  "model": "llama-3.3-70b",
  "tokens_used": 412
}
```

The tool-calling steps are invisible to the caller. You send a question and receive a complete answer.

---

## Example: Multiple Tool Calls in One Turn

Models can request multiple tools simultaneously. For example, a research agent asked to "Compare the population of Tokyo and New York" might request two web searches in parallel:

```json
{
  "tool_calls": [
    {"name": "web_search", "arguments": {"query": "Tokyo population 2026"}},
    {"name": "web_search", "arguments": {"query": "New York population 2026"}}
  ]
}
```

With `parallel_execution` enabled (the default), both searches execute concurrently. The results are sent back to the model together, and it produces a response comparing both cities.

---

## Example: Multi-Turn Tool Usage

Some questions require multiple rounds of tool use. For example:

**User:** "What is 15% of the GDP of France?"

**Turn 1 — Agent calls web_search:**
```json
{"name": "web_search", "arguments": {"query": "France GDP 2026 USD"}}
```
Result: France's GDP is approximately $3.1 trillion.

**Turn 2 — Agent calls calculator:**
```json
{"name": "calculator", "arguments": {"expression": "3100000000000 * 0.15"}}
```
Result: 465,000,000,000

**Turn 3 — Agent produces final response:**
"15% of France's GDP (approximately $3.1 trillion) is **$465 billion**."

Each round counts toward the `max_iterations` limit.

---

## Error Handling

If a tool call fails (timeout, invalid input, etc.), ARES returns an error result to the model:

```json
{
  "tool_result": {
    "name": "web_search",
    "error": "Search timed out after 30 seconds"
  }
}
```

The model can then decide to:
- Retry the tool call with different parameters
- Use a different tool
- Respond with what it knows, noting the tool failure

Well-designed system prompts should instruct the agent on how to handle tool failures gracefully.
