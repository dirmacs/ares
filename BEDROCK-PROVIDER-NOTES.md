# Bedrock Provider Notes

## Changed files

- `/opt/ares/Cargo.toml`
- `/opt/ares/ares.example.toml`
- `/opt/ares/crates/ares-config/src/toml_config.rs`
- `/opt/ares/crates/ares-db/src/runtime_providers.rs`
- `/opt/ares/crates/ares-llm/Cargo.toml`
- `/opt/ares/crates/ares-llm/src/bedrock.rs`
- `/opt/ares/crates/ares-llm/src/capabilities.rs`
- `/opt/ares/crates/ares-llm/src/client.rs`
- `/opt/ares/crates/ares-llm/src/lib.rs`
- `/opt/ares/crates/ares-llm/src/provider_registry.rs`
- `/opt/ares/src/api/handlers/admin.rs`
- `/opt/ares-dirmacs/Cargo.toml` (wrapper feature list only)

## Feature flags

- `ares-llm`: `bedrock = ["dep:reqwest"]`
- `ares-server`: `bedrock = ["ares-llm/bedrock"]`
- Bundles: `all-llm = ["openai", "bedrock"]`; `full` includes `bedrock`
- Live wrapper feature set: `postgres,openai,bedrock,ares-vector,mcp,email,search-tools,skills`

## Agent model string

Recommended direct agent config:

```json
{
  "model": "bedrock/us.anthropic.claude-haiku-4-5-20251001-v1:0",
  "tools": ["calculator"],
  "max_tool_iterations": 5
}
```

Also supported: raw Bedrock Anthropic model ids such as `us.anthropic.claude-haiku-4-5-20251001-v1:0`, or a tenant model tier whose `provider_name` is `bedrock` and whose `model_name` is the raw Bedrock model id.

## Environment variables

- `AWS_BEARER_TOKEN_BEDROCK`
- `AWS_REGION`
- `BEDROCK_MODEL` is optional and only used by `Provider::from_env`; tenant agent routing should use the configured model or tier.

## Admin API setup

Headers for admin calls:

```text
X-Admin-Secret: <admin secret>
Content-Type: application/json
```

Check the deployed build exposes Bedrock:

```http
GET /api/admin/fleet-providers/capabilities
```

Allow the raw model id for the tenant:

```http
POST /api/admin/tenants/{tenant_id}/allowed-models

{"model_id":"us.anthropic.claude-haiku-4-5-20251001-v1:0"}
```

Optional tier mapping:

```http
PUT /api/admin/tenants/{tenant_id}/model-tiers/powerful

{"provider_name":"bedrock","model_name":"us.anthropic.claude-haiku-4-5-20251001-v1:0"}
```

Point an agent directly at Bedrock:

```http
PUT /api/admin/tenants/{tenant_id}/agents/{agent_name}

{"config":{"model":"bedrock/us.anthropic.claude-haiku-4-5-20251001-v1:0","tools":["calculator"],"max_tool_iterations":5}}
```

Create a new tenant agent with Bedrock if it does not exist:

```http
POST /api/admin/tenants/{tenant_id}/agents

{"agent_name":"bedrock-agent","display_name":"Bedrock Agent","description":null,"config":{"model":"bedrock/us.anthropic.claude-haiku-4-5-20251001-v1:0","tools":["calculator"],"max_tool_iterations":5}}
```

Or point an agent at the tier:

```http
PUT /api/admin/tenants/{tenant_id}/agents/{agent_name}

{"config":{"model":"powerful","tools":["calculator"],"max_tool_iterations":5}}
```

No `/api/admin/runtime_providers` row is required for the built-in env-backed `bedrock` provider.

## Chat test

Use a tenant API key and a prompt that requires a configured tool:

```http
POST /api/v1/chat
Authorization: Bearer <tenant api key>
Content-Type: application/json

{"agent_type":"{agent_name}","message":"Use the calculator tool to compute 314159 * 271828, then answer with the product."}
```

Expected path: ARES resolves the tenant agent, constructs a Bedrock Claude client, receives a `tool_use` block, executes the tool, sends a `tool_result` block back to Bedrock, and returns the final assistant text.

## Manual deployment steps

Human operator only:

1. Put `AWS_BEARER_TOKEN_BEDROCK` and `AWS_REGION` into the deployment environment (`/opt/ares-dirmacs/.env` or `/etc/dirmacs/*.env`).
2. Build the deployable wrapper from `/opt/ares-dirmacs`: `cargo build --release`.
3. Restart the live unit: `sudo systemctl restart ares`.

This change was built but not deployed or restarted.
