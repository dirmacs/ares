# Azure AI Foundry Provider Notes

## What changed

- Added root feature `azure` and `ares-llm` feature `azure` (`azure` enables the existing OpenAI-compatible client path).
- Added `crates/ares-llm/src/azure.rs` with Foundry defaults, base URL normalization, `azure/` model-prefix stripping, and dual Foundry headers: `api-key` plus `Authorization: Bearer`.
- Added `Provider::Azure` and `ProviderConfig::Azure`.
- Wired `azure` into `ProviderRegistry`, model capabilities, model listing, runtime provider validation, fleet provider capabilities, and `ares.example.toml`.
- Azure Foundry is intentionally a thin wrapper over the existing OpenAI chat-completions client because Foundry uses the standard OpenAI `/chat/completions` body, including `tools`.

## Build Feature

Use the `azure` feature:

```bash
cargo build --no-default-features --features postgres,openai,bedrock,azure,ares-vector,mcp,email,search-tools,skills
```

## Model String

Direct model routing:

```text
azure/DeepSeek-V4-Flash
```

Tenant-tier routing:

```json
{"model":"azure"}
```

where tenant model tier `azure` maps to provider `azure` and model `DeepSeek-V4-Flash`.

## Environment Variables

ARES reads:

```text
AZURE_FOUNDRY_API_KEY
AZURE_FOUNDRY_BASE_URL
AZURE_FOUNDRY_MODEL
```

`AZURE_FOUNDRY_MODEL` is optional; default is `DeepSeek-V4-Flash`.

Current live ProPrez values are named `AZURE_DEEPSEEK_API_KEY`, `AZURE_DEEPSEEK_BASE_URL`, and `AZURE_DEEPSEEK_MODEL`; deployment should map those values into the ARES names above.

`AZURE_FOUNDRY_BASE_URL` should be the OpenAI-compatible Foundry base, for example:

```text
https://<resource>.services.ai.azure.com/openai/v1
```

## Admin API Setup

Set a tenant model tier:

```bash
curl -X PUT "$BASE/api/admin/tenants/<tenant_id>/model-tiers/azure" \
  -H "X-Admin-Secret: $ADMIN_SECRET" \
  -H "Content-Type: application/json" \
  -d '{"provider_name":"azure","model_name":"DeepSeek-V4-Flash"}'
```

Point an agent at that tier:

```bash
curl -X PUT "$BASE/api/admin/tenants/<tenant_id>/agents/<agent_name>" \
  -H "X-Admin-Secret: $ADMIN_SECRET" \
  -H "Content-Type: application/json" \
  -d '{
    "config": {
      "model": "azure",
      "system_prompt": "You are a tool-using assistant.",
      "tools": ["<tool_name>"],
      "max_tool_iterations": 5
    },
    "enabled": true
  }'
```

Or skip the tier and route directly:

```json
{"model":"azure/DeepSeek-V4-Flash","tools":["<tool_name>"],"max_tool_iterations":5}
```

The static provider path above is preferred for this rollout. A runtime provider row with `provider_type: "azure"` or `"azure-compatible"` is also accepted by validation, but the first-class `azure` provider already exists when the binary is built with `azure`.

## Chat Test

Call the tenant-scoped API using that tenant's ARES API key:

```bash
curl -X POST "$BASE/api/v1/chat" \
  -H "Authorization: Bearer <ares_api_key>" \
  -H "Content-Type: application/json" \
  -d '{
    "agent_type": "orchestrator",
    "message": "Use an available tool if it helps: what can you do?"
  }'
```

Expected headers include `x-provider-name: azure` and `x-model-name: DeepSeek-V4-Flash` when the Azure tier/direct model is selected.

## Manual Deploy Steps

1. Build in the deployment tree with:
   `--no-default-features --features postgres,openai,bedrock,azure,ares-vector,mcp,email,search-tools,skills`.
2. Set `AZURE_FOUNDRY_API_KEY` and `AZURE_FOUNDRY_BASE_URL`; optionally set `AZURE_FOUNDRY_MODEL`.
3. Ensure the `ares.toml` provider block is present only if you want explicit config; the `azure` feature also registers a default `azure` provider using the env var names above.
4. Use the admin calls above to map the tenant/agent to Azure.
5. Test through `POST /api/v1/chat`.

Do not run service restarts or database edits from this worktree; a human should perform deploy/restart steps.
