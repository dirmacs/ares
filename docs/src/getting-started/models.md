# Models & providers

ARES routes LLM requests across multiple providers through a single API. You do not call providers directly. ARES selects the model from the agent configuration and handles credentials, rate limits, and failover.

## Available models

| Tier | Provider | Model | Best for |
|---|---|---|---|
| `fast` | Groq | `llama-3.1-8b-instant` | Quick responses, classification, simple Q&A |
| `balanced` | Groq | `llama-3.3-70b-versatile` | General-purpose tasks, GPT-4 class quality |
| `powerful` | Anthropic | `claude-3.5-sonnet` | Complex reasoning, long-form analysis, nuanced tasks |
| `deepseek` | NVIDIA | `deepseek-r1-distill-llama-70b` | Code generation, technical documentation, structured output |
| `local` | Ollama | `ministral-3:3b` | Development, testing, offline use |

## How model selection works

You do not name a model in your API calls. You pass an `agent_type`, and each agent has a configured model tier.

```bash
# This request is routed to whichever model the "product" agent is configured to use
curl -X POST http://localhost:3000/v1/chat \
  -H "Authorization: Bearer ares_xxx" \
  -H "Content-Type: application/json" \
  -d '{"message": "Compare these two options", "agent_type": "product"}'
```

Your tenant administrator configures the mapping between agents and models. A typical setup looks like this:

| Agent | Model tier | Rationale |
|---|---|---|
| `classifier` | `fast` | Needs speed, not depth |
| `product` | `balanced` | General-purpose, good quality |
| `analyst` | `powerful` | Complex reasoning required |
| `code-review` | `deepseek` | Specialized for code tasks |

With this design, you upgrade the model behind an agent without changes to client code.

## Provider architecture

ARES uses a named-provider system. Each provider has a configured API endpoint, credentials, and rate limits. Models reference their provider by name.

```
┌─────────────┐
│  Your App   │
│  agent_type │
└──────┬──────┘
       │
       ▼
┌─────────────┐     ┌──────────┐
│    ARES     │────▶│   Groq   │  fast, balanced
│   Router    │     └──────────┘
│             │     ┌──────────┐
│             │────▶│Anthropic │  powerful
│             │     └──────────┘
│             │     ┌──────────┐
│             │────▶│  NVIDIA  │  deepseek
│             │     └──────────┘
│             │     ┌──────────┐
│             │────▶│  Ollama  │  local
└─────────────┘     └──────────┘
```

### Provider details

**Groq**: high-throughput inference on custom LPUs with fast response times. It hosts open-source models (Llama, Mixtral). A free tier is available with rate limits.

**Anthropic**: Claude models. These models lead in complex reasoning, instruction following, and safety. This provider requires a paid API key.

**NVIDIA (DeepSeek)**: NVIDIA-hosted DeepSeek models over the NVIDIA AI API. They are strong at code generation and structured technical output.

**Ollama**: self-hosted local inference with no external API calls. It fits development, air-gapped environments, or on-premises data requirements.

## Rate limits

ARES enforces rate limits per provider and per tenant. These are the default limits for the Groq free tier:

| Model tier | Requests per day | Tokens per minute |
|---|---|---|
| `fast` (llama-3.1-8b) | 14,400 | 20,000 |
| `balanced` (llama-3.3-70b) | 6,000 | 6,000 |

Anthropic and NVIDIA rate limits depend on your API plan with those providers. ARES surfaces provider rate limit errors unchanged:

```json
{
  "error": "Rate limit exceeded for provider 'groq'",
  "code": "RATE_LIMIT_EXCEEDED",
  "retry_after": 60
}
```

Your administrator configures tenant-level rate limits and quotas separately. ARES enforces them regardless of provider limits.

## Adding your own providers

If you self-host ARES, add providers to your `ares.toml` configuration:

```toml
[[providers]]
name = "my-openai"
kind = "openai"
api_base = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

[[models]]
name = "gpt-4o"
provider = "my-openai"
model_id = "gpt-4o"
tier = "powerful"
```

Any provider that exposes an OpenAI-compatible API (vLLM, Together AI, Fireworks, and more) works through the `openai` provider kind.

## Choosing the right tier

| If you need... | Use tier |
|---|---|
| Fastest possible response | `fast` |
| Good quality at reasonable speed | `balanced` |
| Maximum reasoning capability | `powerful` |
| Code generation or technical tasks | `deepseek` |
| Offline or local development | `local` |

Start with `balanced` when unsure. For most use cases it gives the best trade-off between quality, speed, and cost.
