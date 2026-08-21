# Introduction

ARES is a multi-provider LLM platform that gives you a single, unified API to route requests across Groq, Anthropic, NVIDIA DeepSeek, and Ollama. It handles tool calling, retrieval-augmented generation (RAG), multi-step workflows, streaming, usage metering, and multi-tenant isolation out of the box — so you can focus on building your AI application instead of stitching together provider SDKs.

> **0.8.0 Cordis redesign (2026-08-21)** — The runtime is now Cordis-informed: unified `Context{store+isolate+intercept+fiber+parent+root}` with witnessed LIFO effects, `TypeId`-keyed coherent table, Fiber states + `:uid` epoch watch, Events 5 modes, Loader `EntryTree` reconcile, and 8-plugin wiring `root_ctx.plugin(ConfigService).plugin(CatalogService).plugin(ProviderRegistryService).plugin(AuthServiceWrapper).plugin(AgentServiceWrapper).plugin(ToolServiceWrapper).plugin(SchedulerService).plugin(HealthJobService)` (plus `PipelineService`/`TriggerService`/`SkillsService`/`WorkflowService`). 177 handlers migrated `State<AppState> → State<Arc<Context>>` + `ctx.get::<Service>()`, `admin.rs` 3059→165 thin shards (15 files), `cfg(feature)` 0 in handlers, HMR file-watch 500 ms debounce. See [Architecture](./platform/architecture.md) and the Cordis chapters below for the full mapping.

## Key capabilities

- **Multi-provider LLM routing** — Send requests to Groq, Anthropic, NVIDIA, or Ollama through one API. Switch models without changing your integration.
- **Tool calling** — Define tools your agents can invoke. ARES manages the tool-call loop, execution, and response assembly.
- **Retrieval-augmented generation (RAG)** — Ground LLM responses in your own data with built-in retrieval pipelines.
- **Workflows** — Chain multiple agents and processing steps into deterministic, multi-step workflows.
- **Multi-tenant enterprise support** — Tenant isolation, per-tenant agent configuration, API key scoping, and usage tracking at the tenant level.
- **Streaming** — Server-Sent Events (SSE) streaming for real-time, token-by-token responses.
- **Usage metering** — Track tokens, requests, and costs per tenant with built-in rate limiting and quota enforcement.
- **Skills** — SKILL.md file discovery and loading via [thulp-skill-files](https://crates.io/crates/thulp-skill-files). Scope-based priority resolution (project > personal > plugin).
- **MCP integration** — Bridge external MCP servers as agent-callable tools. Connect Eruka, Daedra, or any MCP-compatible service.
- **Loop detection** — Sliding-window hash tracking with 3-tier escalation (warn, force alternative, halt) prevents agents from getting stuck in infinite loops.
- **Crash recovery** — Checkpoint-based state serialization lets agents resume from the last saved state after failures.
- **Agent versioning** — Version history, rollback, and emergency stop (kill switch) for all agent requests.
- **Research coordination** — Deep research agent with configurable depth and max iterations for multi-step investigation tasks.
- **Deployment automation** — Built-in deploy/rollback endpoints with service health monitoring and log streaming.

## Who is ARES for?

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
