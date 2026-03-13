# Workflows

Workflows are multi-agent orchestration pipelines. A workflow defines an entry point agent (typically a router) that analyzes the incoming query and delegates to specialist agents in sequence. The result is a coordinated, multi-step response that leverages the strengths of different agents.

**How workflows operate:**

1. The query enters through an **entry agent** (usually a router).
2. The router analyzes intent and selects the most appropriate specialist agent.
3. The specialist processes the query, optionally delegating further.
4. Each step is recorded in the **reasoning path**, providing full transparency into the decision chain.
5. The final response is returned along with metadata about the execution.

---

## List workflows

```
GET /api/workflows
```

Returns the names of all available workflows.

### Authentication

Requires a JWT access token: `Authorization: Bearer <jwt_access_token>`

### Response

```json
["default", "research", "support"]
```

### Example

```bash
curl https://api.ares.dirmacs.com/api/workflows \
  -H "Authorization: Bearer eyJhbGciOi..."
```

---

## Execute a workflow

```
POST /api/workflows/{workflow_name}
```

Execute a named workflow. The query is routed through the workflow's agent chain, and the final synthesized response is returned along with execution metadata.

### Authentication

Requires a JWT access token: `Authorization: Bearer <jwt_access_token>`

### Path parameters

| Parameter       | Type   | Description                     |
|----------------|--------|---------------------------------|
| `workflow_name` | string | Name of the workflow to execute |

### Request body

| Parameter | Type   | Required | Description                                           |
|-----------|--------|----------|-------------------------------------------------------|
| `query`   | string | Yes      | The input query or task for the workflow.              |
| `context`  | object | No       | Additional context passed to agents during execution. |

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

| Field            | Type     | Description                                                  |
|-----------------|----------|--------------------------------------------------------------|
| `final_response` | string   | The synthesized response from the workflow.                  |
| `steps_executed` | integer  | Total number of agent steps in the execution.                |
| `agents_used`    | string[] | Ordered list of agents that participated.                    |
| `reasoning_path` | array    | Step-by-step trace of each agent's reasoning and actions.    |

### Examples

#### curl

```bash
curl -X POST https://api.ares.dirmacs.com/api/workflows/default \
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
    "https://api.ares.dirmacs.com/api/workflows/default",
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
  "https://api.ares.dirmacs.com/api/workflows/default",
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

**Agent selection.** The entry agent examines the query and routes to the specialist best suited to handle it. If a specialist determines it needs input from another agent, it can delegate further, creating a multi-hop chain.

**Context propagation.** The optional `context` object is available to every agent in the chain. Use it to pass structured information (user tier, session metadata, domain-specific parameters) that agents can reference during processing.

**Determinism.** Workflow routing is driven by the entry agent's LLM reasoning, so the same query may route differently depending on phrasing. The `reasoning_path` in the response provides full visibility into routing decisions.
