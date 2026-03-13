# Models & Providers

ARES routes LLM requests across multiple providers through a single API. You do not call providers directly — ARES selects the appropriate model based on the agent configuration and handles credentials, rate limits, and failover transparently.

## Available models

| Tier | Provider | Model | Best for |
|---|---|---|---|
| `fast` | Groq | `llama-3.1-8b-instant` | Quick responses, classification, simple Q&A |
| `balanced` | Groq | `llama-3.3-70b-versatile` | General-purpose tasks, GPT-4 class quality |
| `powerful` | Anthropic | `claude-sonnet-4-6` | Complex reasoning, long-form analysis, nuanced tasks |
| `deepseek` | NVIDIA | `deepseek-v3.2` | Code generation, technical documentation, structured output |
| `local` | Ollama | `ministral-3:3b` | Development, testing, offline use |

## How model selection works

You do not specify a model directly in your API calls. Instead, you specify an `agent_type`, and each agent is configured with a model tier.

```bash
# This request is routed to whichever model the "product" agent is configured to use
curl -X POST https://api.ares.dirmacs.com/v1/chat \
  -H "Authorization: Bearer ares_xxx" \
  -H "Content-Type: application/json" \
  -d '{"message": "Compare these two options", "agent_type": "product"}'
```

The mapping between agents and models is configured by your tenant administrator. A typical setup might look like:

| Agent | Model tier | Rationale |
|---|---|---|
| `classifier` | `fast` | Needs speed, not depth |
| `product` | `balanced` | General-purpose, good quality |
| `analyst` | `powerful` | Complex reasoning required |
| `code-review` | `deepseek` | Specialized for code tasks |

This design means you can upgrade an agent's underlying model without changing any client code.

## Provider architecture

ARES uses a named-provider system. Each provider is configured with its API endpoint, credentials, and rate limits. Models reference their provider by name.

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

**Groq** — High-throughput inference on custom LPUs. Extremely fast response times. Hosts open-source models (Llama, Mixtral). Free tier available with rate limits.

**Anthropic** — Claude models. Best-in-class for complex reasoning, instruction following, and safety. Requires a paid API key.

**NVIDIA (DeepSeek)** — NVIDIA-hosted DeepSeek models via the NVIDIA AI API. Strong at code generation and structured technical output.

**Ollama** — Self-hosted, local inference. No external API calls. Useful for development, air-gapped environments, or when you need to keep data on-premises.

## Rate limits

Rate limits are enforced per provider and per tenant. The following are default limits for the Groq free tier:

| Model tier | Requests per day | Tokens per minute |
|---|---|---|
| `fast` (llama-3.1-8b) | 14,400 | 20,000 |
| `balanced` (llama-3.3-70b) | 6,000 | 6,000 |

Anthropic and NVIDIA rate limits depend on your API plan with those providers. ARES surfaces rate limit errors transparently:

```json
{
  "error": "Rate limit exceeded for provider 'groq'",
  "code": "RATE_LIMIT_EXCEEDED",
  "retry_after": 60
}
```

Tenant-level rate limits and quotas are configured separately by your administrator and enforced by ARES regardless of provider limits.

## Adding your own providers

If you are self-hosting ARES, you can add providers in your `ares.toml` configuration:

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

Any provider that exposes an OpenAI-compatible API (vLLM, Together AI, Fireworks, etc.) can be added using the `openai` provider kind.

## Choosing the right tier

| If you need... | Use tier |
|---|---|
| Fastest possible response | `fast` |
| Good quality at reasonable speed | `balanced` |
| Maximum reasoning capability | `powerful` |
| Code generation or technical tasks | `deepseek` |
| Offline or local development | `local` |

When in doubt, start with `balanced`. It provides the best trade-off between quality, speed, and cost for most use cases.
