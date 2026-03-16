# Research

The Research API performs deep, multi-step research on a topic using parallel sub-agents. Unlike a single chat request, a research query spawns multiple agents that independently explore facets of the question, synthesize findings, and produce a comprehensive result with source attribution.

---

## Execute a research query

```
POST /api/research
```

Submit a research query for deep, multi-step investigation.

### Authentication

Requires a JWT access token: `Authorization: Bearer <jwt_access_token>`

### Request body

| Parameter        | Type    | Required | Default | Description                                                             |
|-----------------|---------|----------|---------|-------------------------------------------------------------------------|
| `query`          | string  | Yes      | --      | The research question or topic.                                         |
| `depth`          | integer | No       | 3       | How many levels deep the research goes. Higher values explore sub-topics more thoroughly. |
| `max_iterations` | integer | No       | 5       | Maximum total agent calls. Acts as a cost/time ceiling.                 |

**Understanding `depth`:** At depth 1, the research agent answers the query directly. At depth 2, it identifies sub-questions, spawns agents to answer each, then synthesizes. At depth 3+, sub-agents can spawn their own sub-agents, creating a tree of investigation.

**Understanding `max_iterations`:** This is a hard cap on total agent invocations across all depth levels. If the research tree would require more calls than `max_iterations`, it stops expanding and synthesizes what it has. Use this to control cost and response time.

### Response

```json
{
  "findings": "## Market Analysis: Edge Computing in Healthcare\n\nEdge computing adoption in healthcare is accelerating, driven by three primary factors...\n\n### Key Findings\n1. **Latency requirements** — Real-time patient monitoring demands sub-10ms response times...\n2. **Data sovereignty** — HIPAA compliance increasingly favors on-premise processing...\n3. **Cost dynamics** — Edge deployment reduces cloud egress costs by 40-60% for imaging workloads...\n\n### Sources\n- Gartner Healthcare IT Report 2025\n- IEEE Edge Computing Survey\n- HHS HIPAA Guidance Update",
  "sources": [
    "Gartner Healthcare IT Report 2025",
    "IEEE Edge Computing Survey",
    "HHS HIPAA Guidance Update"
  ],
  "duration_ms": 8432
}
```

| Field         | Type     | Description                                             |
|--------------|----------|---------------------------------------------------------|
| `findings`    | string   | The synthesized research output, typically in Markdown. |
| `sources`     | string[] | References and sources discovered during research.      |
| `duration_ms` | integer  | Total time taken for the research in milliseconds.      |

### Examples

#### curl

```bash
curl -X POST https://api.ares.dirmacs.com/api/research \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -d '{
    "query": "What are the current trends in edge computing for healthcare?",
    "depth": 3,
    "max_iterations": 5
  }'
```

#### Python

```python
import requests

response = requests.post(
    "https://api.ares.dirmacs.com/api/research",
    headers={
        "Content-Type": "application/json",
        "Authorization": "Bearer eyJhbGciOi..."
    },
    json={
        "query": "What are the current trends in edge computing for healthcare?",
        "depth": 3,
        "max_iterations": 5
    }
)

result = response.json()
print(result["findings"])
print(f"\nCompleted in {result['duration_ms']}ms")
print(f"Sources: {', '.join(result['sources'])}")
```

#### JavaScript

```javascript
const response = await fetch("https://api.ares.dirmacs.com/api/research", {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
    "Authorization": "Bearer eyJhbGciOi..."
  },
  body: JSON.stringify({
    query: "What are the current trends in edge computing for healthcare?",
    depth: 3,
    max_iterations: 5
  })
});

const result = await response.json();
console.log(result.findings);
console.log(`\nCompleted in ${result.duration_ms}ms`);
console.log(`Sources: ${result.sources.join(", ")}`);
```

---

## Tuning research parameters

| Scenario                          | Recommended `depth` | Recommended `max_iterations` |
|----------------------------------|---------------------|------------------------------|
| Quick factual lookup             | 1                   | 2                            |
| Standard research question       | 2                   | 5                            |
| Deep competitive analysis        | 3                   | 10                           |
| Exhaustive literature review     | 4+                  | 15+                          |

Higher depth and iteration values produce more comprehensive results but take longer and consume more API quota. For most use cases, the defaults (`depth: 3`, `max_iterations: 5`) provide a good balance of thoroughness and speed.
