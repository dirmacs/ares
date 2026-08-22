# Introduction

ARES is a multi-provider LLM platform that provides one API to route requests to Groq, Anthropic, NVIDIA DeepSeek, and Ollama. It handles tool calling, retrieval-augmented generation (RAG), multi-step workflows, streaming, usage metering, and multi-tenant isolation, so you can build your application instead of wiring provider SDKs together.

> **0.9.0 (2026-08-22):** Service-based architecture with dependency injection. Components register into a typed `Context`, handlers pull deps with `ctx.get::<T>()`. Unified `Execute` handles all execution paths through event-first around-middleware (`EventsService::waterfall_around` on `tools.*`, `llm.*`, and `agent.run`). Skills keep the request `Context`, isolate tools per tenant with `ctx.isolate::<Tools>(tenant_id)`, and run tool steps through `Tools::execute`. Skill `LlmCall` steps use the strict evented `Llm::complete` (`llm.complete`) path, with no direct provider `generate_with_history` fallback. Hot-reload via file-watch. Circuit breaker on LLM providers.

## Key capabilities

- Multi-provider LLM routing: send requests to Groq, Anthropic, NVIDIA, or Ollama through one API. Switch models without changing your integration.
- Tool calling: define tools your agents can invoke. ARES runs the tool-call loop, execution, and response assembly.
- Retrieval-augmented generation (RAG): ground LLM responses in your own data with built-in retrieval pipelines.
- Workflows: chain agents and processing steps into deterministic, multi-step workflows.
- Multi-tenant enterprise support: tenant isolation, per-tenant agent configuration, API key scoping, and usage tracking at the tenant level.
- Streaming: Server-Sent Events (SSE) streaming for real-time, token-by-token responses.
- Usage metering: track tokens, requests, and costs per tenant with built-in rate limiting and quota enforcement.
- Skills: SKILL.md file discovery and loading via [thulp-skill-files](https://crates.io/crates/thulp-skill-files). Scope-based priority resolution (project > personal > plugin).
- MCP integration: bridge external MCP servers as agent-callable tools. Connect Eruka, Daedra, or any MCP-compatible service.
- Loop detection: sliding-window hash tracking with 3-tier escalation (warn, force alternative, halt) prevents agents from getting stuck in loops.
- Crash recovery: checkpoint-based state serialization lets agents resume from the last saved state after failures.
- Agent versioning: version history, rollback, and emergency stop (kill switch) for all agent requests.
- Research coordination: deep research agent with configurable depth and max iterations for multi-step investigation tasks.
- Deployment automation: built-in deploy and rollback endpoints with service health monitoring and log streaming.

## Who is ARES for

- **Platform teams** building internal AI infrastructure who need a reliable, multi-provider abstraction layer.
- **Enterprise clients** who want managed AI agents with tenant isolation, usage visibility, and SLA guarantees.
- **Developers building AI applications** who want a clean API without managing provider credentials, rate limits, and failover logic themselves.

## Base URL

All API requests are made to:

```
http://localhost:3000
```

## Quick links

| Resource | Description |
|---|---|
| [Quickstart](getting-started/quickstart.md) | Zero to first API call in 5 minutes |
| [Authentication](getting-started/authentication.md) | API keys, JWT tokens, and admin auth |
| [Models & Providers](getting-started/models.md) | Available models, tiers, and provider configuration |
| [Changelog](changelog.md) | Release history and breaking changes |
