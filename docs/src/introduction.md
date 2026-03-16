# Introduction

ARES is a multi-provider LLM platform that gives you a single, unified API to route requests across Groq, Anthropic, NVIDIA DeepSeek, and Ollama. It handles tool calling, retrieval-augmented generation (RAG), multi-step workflows, streaming, usage metering, and multi-tenant isolation out of the box — so you can focus on building your AI application instead of stitching together provider SDKs.

## Key capabilities

- **Multi-provider LLM routing** — Send requests to Groq, Anthropic, NVIDIA, or Ollama through one API. Switch models without changing your integration.
- **Tool calling** — Define tools your agents can invoke. ARES manages the tool-call loop, execution, and response assembly.
- **Retrieval-augmented generation (RAG)** — Ground LLM responses in your own data with built-in retrieval pipelines.
- **Workflows** — Chain multiple agents and processing steps into deterministic, multi-step workflows.
- **Multi-tenant enterprise support** — Tenant isolation, per-tenant agent configuration, API key scoping, and usage tracking at the tenant level.
- **Streaming** — Server-Sent Events (SSE) streaming for real-time, token-by-token responses.
- **Usage metering** — Track tokens, requests, and costs per tenant with built-in rate limiting and quota enforcement.

## Who is ARES for?

- **Platform teams** building internal AI infrastructure who need a reliable, multi-provider abstraction layer.
- **Enterprise clients** who want managed AI agents with tenant isolation, usage visibility, and SLA guarantees.
- **Developers building AI applications** who want a clean API without managing provider credentials, rate limits, and failover logic themselves.

## Base URL

All API requests are made to:

```
https://api.ares.dirmacs.com
```

## Quick links

| Resource | Description |
|---|---|
| [Quickstart](getting-started/quickstart.md) | Zero to first API call in 5 minutes |
| [Authentication](getting-started/authentication.md) | API keys, JWT tokens, and admin auth |
| [Models & Providers](getting-started/models.md) | Available models, tiers, and provider configuration |
| [Changelog](changelog.md) | Release history and breaking changes |
