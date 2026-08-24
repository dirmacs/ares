# Workflows

Workflows are multi-agent orchestration pipelines. A workflow defines an entry agent (usually a router). The entry agent analyzes the query and delegates to specialist agents in sequence. The result is a coordinated multi-step response that uses the strengths of different agents.

**How workflows operate:**

1. The query enters through an **entry agent** (usually a router).
2. The router analyzes the intent and selects a specialist agent.
3. The specialist processes the query and can delegate further steps to other agents.
4. The system records each step in the **reasoning path**. The reasoning path shows the full decision chain.
5. The system returns the final response with metadata about the execution.

---

## List workflows

```
GET /api/workflows
```

This endpoint returns the names of all available workflows.

### Authentication

Requires a JWT access token: `Authorization: Bearer <jwt_access_token>`

### Response

```json
["default", "research", "support"]
```

### Example

```bash
curl http://localhost:3000/api/workflows \
  -H "Authorization: Bearer eyJhbGciOi..."
```

---

## Execute a workflow

```
POST /api/workflows/{workflow_name}
```

This endpoint runs a named workflow. ARES routes the query through the agent chain of the workflow and returns the final response with execution metadata.

### Authentication

Requires a JWT access token: `Authorization: Bearer <jwt_access_token>`

### Path parameters

| Parameter | Type | Description |
|----------------|--------|---------------------------------|
| `workflow_name` | string | Name of the workflow to run |

### Request body

| Parameter | Type | Required | Description |
|-----------|--------|----------|-------------------------------------------------------|
| `query` | string | Yes | The input query or task for the workflow. |
| `context` | object | No | Additional context passed to agents during execution. |

### Response

```json
{
  "final_response": "Based on our analysis, the Pro plan at $49/month offers the best value for your use case. It includes 100K API calls, priority support, and access to all models. The Enterprise plan adds dedicated infrastructure and SLA guarantees, which may be worth considering if you expect to exceed 500K calls/month.",
  "steps_executed": 3,
  "agents_used": ["router", "sales", "product"],
  "reasoning_path": [
    {
      "agent": "router",
      "action": "Classified as pricing inquiry. Routing to sales agent."
    },
    {
      "agent": "sales",
      "action": "Retrieved pricing tiers. Consulting product agent for feature comparison."
    },
    {
      "agent": "product",
      "action": "Compared Pro vs Enterprise feature sets. Synthesized final recommendation."
    }
  ]
}
```

| Field | Type | Description |
|-----------------|----------|--------------------------------------------------------------|
| `final_response` | string | The synthesized response from the workflow. |
| `steps_executed` | integer | Total number of agent steps in the execution. |
| `agents_used` | string[] | Ordered list of agents that participated. |
| `reasoning_path` | array | Step-by-step trace of each agent's reasoning and actions. |

### Examples

#### curl

```bash
curl -X POST http://localhost:3000/api/workflows/default \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -d '{
    "query": "Compare your Pro and Enterprise pricing plans for a mid-size SaaS company",
    "context": {
      "company_size": "50-200 employees",
      "expected_volume": "200K calls/month"
    }
  }'
```

#### Python

```python
import requests

response = requests.post(
    "http://localhost:3000/api/workflows/default",
    headers={
        "Content-Type": "application/json",
        "Authorization": "Bearer eyJhbGciOi..."
    },
    json={
        "query": "Compare your Pro and Enterprise pricing plans for a mid-size SaaS company",
        "context": {
            "company_size": "50-200 employees",
            "expected_volume": "200K calls/month"
        }
    }
)

result = response.json()
print(result["final_response"])

# Inspect the reasoning chain
for step in result["reasoning_path"]:
    print(f"  [{step['agent']}] {step['action']}")
```

#### JavaScript

```javascript
const response = await fetch(
  "http://localhost:3000/api/workflows/default",
  {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Authorization": "Bearer eyJhbGciOi..."
    },
    body: JSON.stringify({
      query: "Compare your Pro and Enterprise pricing plans for a mid-size SaaS company",
      context: {
        company_size: "50-200 employees",
        expected_volume: "200K calls/month"
      }
    })
  }
);

const result = await response.json();
console.log(result.final_response);

// Inspect the reasoning chain
result.reasoning_path.forEach(step => {
  console.log(`  [${step.agent}] ${step.action}`);
});
```

---

## Workflow behavior

**Agent selection.** The entry agent examines the query and routes it to a specialist that can handle it. When a specialist needs input from another agent, the specialist delegates to that agent. This delegation creates a multi-hop chain.

**Context propagation.** Every agent in the chain can read the optional `context` object. Use this object to pass structured information: user tier, session metadata, and domain parameters. Agents reference this information during processing.

**Determinism.** Routing depends on the LLM reasoning of the entry agent. The same query can route differently with different phrasing. The `reasoning_path` in the response shows all routing decisions.
