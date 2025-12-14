# Deprecated Agents Migration Guide

**Version**: 0.2.0  
**Date**: 2024-12-15

---

## Overview

In v0.2.0, the legacy hardcoded agents have been **removed** and replaced with `ConfigurableAgent`, which is dynamically created from TOML configuration. This provides:

- ✅ No code changes needed to modify agent behavior
- ✅ Hot-reloading of agent configurations
- ✅ Per-agent model selection
- ✅ Per-agent tool filtering
- ✅ Custom system prompts via config

---

## What Was Removed

The following agent files were removed in v0.2.0:

| File | Agent | Replacement |
|------|-------|-------------|
| `src/agents/product.rs` | `ProductAgent` | `ConfigurableAgent` with `[agents.product]` config |
| `src/agents/invoice.rs` | `InvoiceAgent` | `ConfigurableAgent` with `[agents.invoice]` config |
| `src/agents/sales.rs` | `SalesAgent` | `ConfigurableAgent` with `[agents.sales]` config |
| `src/agents/finance.rs` | `FinanceAgent` | `ConfigurableAgent` with `[agents.finance]` config |
| `src/agents/hr.rs` | `HrAgent` | `ConfigurableAgent` with `[agents.hr]` config |

---

## Migration Steps

### Step 1: Update Your ares.toml

Add agent configurations for any agents you were using:

```toml
# Define your models first
[models.balanced]
provider = "ollama-local"
model = "granite4:tiny-h"
temperature = 0.7
max_tokens = 512

# Then define agents that reference those models
[agents.product]
model = "balanced"
tools = ["calculator"]
system_prompt = """You are a Product Agent specialized in handling product-related queries.

Your responsibilities include:
- Providing product information and specifications
- Helping with product recommendations
- Answering questions about product availability and pricing
- Assisting with product comparisons

Be helpful, accurate, and concise in your responses."""

[agents.invoice]
model = "balanced"
tools = ["calculator"]
system_prompt = """You are an Invoice Agent specialized in handling invoice and billing queries.

Your responsibilities include:
- Processing invoice inquiries
- Explaining billing details
- Helping with payment status
- Resolving billing discrepancies

Be professional and precise with financial information."""

[agents.sales]
model = "balanced"
tools = ["calculator", "web_search"]
system_prompt = """You are a Sales Agent specialized in sales data and analytics.

Your responsibilities include:
- Providing sales performance metrics
- Analyzing sales trends
- Generating sales reports
- Forecasting and projections

Use data to support your insights."""

[agents.finance]
model = "balanced"
tools = ["calculator"]
system_prompt = """You are a Finance Agent specialized in financial analysis.

Your responsibilities include:
- Financial reporting and analysis
- Budget management queries
- Expense tracking
- Financial planning insights

Be precise with numbers and provide clear explanations."""

[agents.hr]
model = "balanced"
tools = []
system_prompt = """You are an HR Agent specialized in human resources queries.

Your responsibilities include:
- Employee policies and procedures
- Benefits information
- Hiring processes
- Workplace guidelines

Be helpful and maintain confidentiality."""
```

### Step 2: Update Any Direct Agent Usage

If you had code that directly instantiated legacy agents:

**Before (v0.1.x)**:
```rust
use ares::agents::product::ProductAgent;

let agent = ProductAgent::new(llm_client);
let response = agent.execute(query, &context).await?;
```

**After (v0.2.0)**:
```rust
use ares::agents::AgentRegistry;

// AgentRegistry is typically available via AppState
let agent = agent_registry.create_agent("product").await?;
let response = agent.execute(query, &context).await?;
```

### Step 3: Use the Chat Endpoint

The recommended approach is to use the HTTP API, which handles agent creation automatically:

```bash
curl -X POST http://localhost:3000/api/chat \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "message": "What products do we have?",
    "agent_type": "product"
  }'
```

Or let the router decide:

```bash
curl -X POST http://localhost:3000/api/chat \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "message": "What products do we have?"
  }'
```

---

## Using Workflows

For multi-agent orchestration, use the workflow engine:

### Define a Workflow

```toml
[workflows.default]
entry_agent = "router"
fallback_agent = "orchestrator"
max_depth = 5
max_iterations = 10
```

### Execute a Workflow

```bash
curl -X POST http://localhost:3000/api/workflows/default \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "What are our Q4 product sales?"
  }'
```

### Workflow Response

```json
{
  "final_response": "Based on the Q4 data...",
  "steps_executed": 3,
  "agents_used": ["router", "sales", "product"],
  "reasoning_path": [
    {
      "agent_name": "router",
      "input": "What are our Q4 product sales?",
      "output": "sales",
      "timestamp": 1702500000,
      "duration_ms": 150
    },
    ...
  ]
}
```

---

## Key Differences

| Aspect | Legacy Agents | ConfigurableAgent |
|--------|--------------|-------------------|
| Definition | Rust code | TOML config |
| Modification | Recompile | Hot-reload |
| System prompt | Hardcoded | Configurable |
| Model | Passed at creation | From config |
| Tools | Fixed | Per-agent config |
| Creation | `AgentName::new()` | `registry.create_agent()` |

---

## Benefits of the New Approach

1. **No Recompilation**: Change agent behavior by editing `ares.toml`
2. **Hot Reloading**: Changes apply within 500ms without restart
3. **Centralized Config**: All agents defined in one place
4. **Tool Filtering**: Restrict which tools each agent can access
5. **Model Selection**: Each agent can use a different model
6. **Workflow Integration**: Agents work seamlessly with workflow engine

---

## Troubleshooting

### Agent Not Found

If you get "Agent 'xxx' not found" errors:

1. Ensure the agent is defined in `ares.toml`
2. Check the agent name matches exactly (case-sensitive)
3. Verify the model referenced by the agent exists

### Missing Model

If you get "Model 'xxx' not found" errors:

1. Ensure the model is defined in `[models.xxx]`
2. Check the provider referenced by the model exists
3. Verify provider configuration is correct

### Tools Not Working

If agent tools aren't being used:

1. Ensure tools are listed in the agent's `tools` array
2. Check tools are enabled in `[tools.xxx]`
3. Verify the model supports tool calling

---

## Questions?

- See `ares.example.toml` for a complete configuration example
- Check `docs/PROJECT_STATUS.md` for implementation details
- Open an issue on GitHub for bugs or feature requests
