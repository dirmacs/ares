# Deprecated Agents Migration Guide

**Version**: 0.2.0  
**Date**: 2024-12-15  
**Updated**: 2024-12-19 (TOON format support)

---

## Overview

In v0.2.0, the legacy hardcoded agents have been **removed** and replaced with `ConfigurableAgent`, which is dynamically created from configuration. This provides:

- ✅ No code changes needed to modify agent behavior
- ✅ Hot-reloading of agent configurations
- ✅ Per-agent model selection
- ✅ Per-agent tool filtering
- ✅ Custom system prompts via config

### Configuration Architecture

A.R.E.S uses a **hybrid TOML + TOON** configuration system:

| Format | File | Purpose | Hot-Reload |
|--------|------|---------|------------|
| **TOML** | `ares.toml` | Infrastructure (server, auth, database, providers) | ✅ Yes |
| **TOON** | `config/*.toon` | Behavioral (agents, models, tools, workflows) | ✅ Yes |

**TOON (Token Oriented Object Notation)** is optimized for LLM consumption with 30-60% token savings.

---

## What Was Removed

The following agent files were removed in v0.2.0:

| File | Agent | Replacement |
|------|-------|-------------|
| `src/agents/product.rs` | `ProductAgent` | `ConfigurableAgent` via `config/agents/product.toon` |
| `src/agents/invoice.rs` | `InvoiceAgent` | `ConfigurableAgent` via `config/agents/invoice.toon` |
| `src/agents/sales.rs` | `SalesAgent` | `ConfigurableAgent` via `config/agents/sales.toon` |
| `src/agents/finance.rs` | `FinanceAgent` | `ConfigurableAgent` via `config/agents/finance.toon` |
| `src/agents/hr.rs` | `HrAgent` | `ConfigurableAgent` via `config/agents/hr.toon` |

---

## Migration Steps

### Step 1: Create TOON Agent Files

Create individual `.toon` files for each agent in `config/agents/`:

**config/agents/product.toon**:
```toon
name: product
model: balanced
tools[1]: calculator
system_prompt: "You are a Product Agent specialized in handling product-related queries.\n\nYour responsibilities include:\n- Providing product information and specifications\n- Helping with product recommendations\n- Answering questions about product availability and pricing\n- Assisting with product comparisons\n\nBe helpful, accurate, and concise in your responses."
```

**config/agents/invoice.toon**:
```toon
name: invoice
model: balanced
tools[1]: calculator
system_prompt: "You are an Invoice Agent specialized in handling invoice and billing queries.\n\nYour responsibilities include:\n- Processing invoice inquiries\n- Explaining billing details\n- Helping with payment status\n- Resolving billing discrepancies\n\nBe professional and precise with financial information."
```

**config/agents/sales.toon**:
```toon
name: sales
model: balanced
tools[2]: calculator,web_search
system_prompt: "You are a Sales Agent specialized in sales data and analytics.\n\nYour responsibilities include:\n- Providing sales performance metrics\n- Analyzing sales trends\n- Generating sales reports\n- Forecasting and projections\n\nUse data to support your insights."
```

**config/agents/finance.toon**:
```toon
name: finance
model: balanced
tools[1]: calculator
system_prompt: "You are a Finance Agent specialized in financial analysis.\n\nYour responsibilities include:\n- Financial reporting and analysis\n- Budget management queries\n- Expense tracking\n- Financial planning insights\n\nBe precise with numbers and provide clear explanations."
```

**config/agents/hr.toon**:
```toon
name: hr
model: balanced
tools[0]:
system_prompt: "You are an HR Agent specialized in human resources queries.\n\nYour responsibilities include:\n- Employee policies and procedures\n- Benefits information\n- Hiring processes\n- Workplace guidelines\n\nBe helpful and maintain confidentiality."
```

### TOON Format Quick Reference

```toon
# Agent configuration
name: my_agent
model: balanced                    # Reference to a model in config/models/
tools[2]: calculator,web_search    # Array with count prefix
max_tool_iterations: 10
parallel_tools: false
system_prompt: "Single line prompt"

# For multiline content, use \n escapes in double quotes:
system_prompt: "Line 1\nLine 2\nLine 3"
```

### Step 2: Ensure Models Are Defined

Models go in `config/models/*.toon`:

**config/models/balanced.toon**:
```toon
name: balanced
provider: ollama-local
model: llama3.2:3b
temperature: 0.7
max_tokens: 2048
```

### Step 3: Update Any Direct Agent Usage

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

### Step 4: Use the Chat Endpoint

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

### Define a Workflow (TOON)

**config/workflows/default.toon**:
```toon
name: default
entry_agent: router
fallback_agent: orchestrator
max_depth: 5
max_iterations: 10
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
    }
  ]
}
```

---

## Directory Structure

```
ares/
├── ares.toml                    # Infrastructure config (TOML)
├── config/                      # Behavioral configs (TOON, hot-reload)
│   ├── agents/
│   │   ├── router.toon
│   │   ├── orchestrator.toon
│   │   ├── product.toon
│   │   ├── invoice.toon
│   │   ├── sales.toon
│   │   ├── finance.toon
│   │   └── hr.toon
│   ├── models/
│   │   ├── fast.toon
│   │   ├── balanced.toon
│   │   └── powerful.toon
│   ├── tools/
│   │   ├── calculator.toon
│   │   └── web_search.toon
│   ├── workflows/
│   │   └── default.toon
│   └── mcps/
│       └── filesystem.toon
└── data/
    └── ares.db
```

---

## Key Differences

| Aspect | Legacy Agents | ConfigurableAgent |
|--------|--------------|-------------------|
| Definition | Rust code | TOON files |
| Modification | Recompile | Hot-reload |
| System prompt | Hardcoded | Configurable |
| Model | Passed at creation | From config |
| Tools | Fixed | Per-agent config |
| Creation | `AgentName::new()` | `registry.create_agent()` |

---

## Benefits of the New Approach

1. **No Recompilation**: Change agent behavior by editing `.toon` files
2. **Hot Reloading**: Changes apply without restart
3. **Token Efficient**: TOON format uses 30-60% fewer tokens than JSON/TOML
4. **Modular**: One file per agent for easy management
5. **Tool Filtering**: Restrict which tools each agent can access
6. **Model Selection**: Each agent can use a different model
7. **Workflow Integration**: Agents work seamlessly with workflow engine

---

## Troubleshooting

### Agent Not Found

If you get "Agent 'xxx' not found" errors:

1. Ensure `config/agents/xxx.toon` exists
2. Check the `name:` field matches the expected agent name
3. Verify the model referenced by the agent exists in `config/models/`

### Missing Model

If you get "Model 'xxx' not found" errors:

1. Ensure `config/models/xxx.toon` exists
2. Check the `provider:` field references a valid provider in `ares.toml`
3. Verify provider configuration is correct

### Tools Not Working

If agent tools aren't being used:

1. Ensure tools are listed in the agent's `tools[n]:` field
2. Check tools are defined in `config/tools/`
3. Verify the model supports tool calling

### TOON Parse Errors

If you see "Multiple values at root level" errors:

1. TOON uses `\n` for newlines in strings, not YAML-style `|` blocks
2. Arrays use count prefix: `tools[2]: a,b` not `tools: [a, b]`
3. Empty arrays: `tools[0]:` (colon required)

---

## Questions?

- See `ares.example.toml` for infrastructure configuration
- See `config/` directory for TOON examples
- Check `docs/PROJECT_STATUS.md` for implementation details
- Check `docs/DIR-12-research.md` for TOON format details
- Open an issue on GitHub for bugs or feature requests
