# Error Handling

ARES uses conventional HTTP status codes and a consistent JSON error format across all endpoints. This page documents the error response structure, status code meanings, and common errors with their solutions.

---

## Error Response Format

All errors return a JSON object with an `error` field containing a human-readable message:

```json
{
  "error": "Human-readable error message"
}
```

The HTTP status code indicates the category of error. The `error` string provides specific details about what went wrong.

---

## HTTP Status Codes

### Success Codes

| Code | Meaning | When Used |
|---|---|---|
| `200` | OK | Successful read or update operation |
| `201` | Created | Resource successfully created (tenant, agent, API key) |
| `204` | No Content | Successful delete with no response body |

### Client Error Codes

| Code | Meaning | When Used |
|---|---|---|
| `400` | Bad Request | Malformed JSON, missing required fields, invalid parameter types |
| `401` | Unauthorized | Missing or invalid authentication credentials |
| `403` | Forbidden | Valid credentials but insufficient permissions for this operation |
| `404` | Not Found | Resource does not exist, or does not belong to your tenant |
| `409` | Conflict | Resource already exists (e.g., duplicate tenant name or agent name) |
| `422` | Unprocessable Entity | Request is well-formed but contains invalid values (e.g., unknown tier, invalid model name) |
| `429` | Too Many Requests | Rate limit or quota exceeded |

### Server Error Codes

| Code | Meaning | When Used |
|---|---|---|
| `500` | Internal Server Error | Unexpected server-side failure |

---

## Common Errors and Solutions

### Authentication Errors

**Missing API key:**
```
HTTP 401
{"error": "Missing authorization header"}
```
Add the `Authorization: Bearer ares_xxx` header to your request.

**Invalid API key:**
```
HTTP 401
{"error": "Invalid API key"}
```
Verify that the API key is correct and has not been revoked. API keys start with `ares_`.

**Missing admin secret:**
```
HTTP 401
{"error": "Missing X-Admin-Secret header"}
```
Admin endpoints require the `X-Admin-Secret` header, not the `Authorization` header.

**Invalid admin secret:**
```
HTTP 401
{"error": "Invalid admin secret"}
```
Verify the admin secret matches the value configured in `ares.toml`.

### Resource Errors

**Agent not found:**
```
HTTP 404
{"error": "Agent not found: risk-analyzer"}
```
The agent does not exist for your tenant. Check the agent name with `GET /v1/agents`. Agent names are case-sensitive.

**Tenant not found:**
```
HTTP 404
{"error": "Tenant not found"}
```
The tenant ID does not exist. List tenants with `GET /api/admin/tenants` to find the correct ID.

**Duplicate resource:**
```
HTTP 409
{"error": "Agent with name 'risk-analyzer' already exists for this tenant"}
```
An agent with this name already exists. Use a different name or update the existing agent.

### Validation Errors

**Invalid tier:**
```
HTTP 422
{"error": "Invalid tier: 'gold'. Valid tiers: free, dev, pro, enterprise"}
```
Use one of the supported tier values.

**Missing required field:**
```
HTTP 400
{"error": "Missing required field: name"}
```
Include all required fields in your request body. Refer to the API documentation for the specific endpoint.

**Invalid JSON:**
```
HTTP 400
{"error": "Invalid JSON in request body"}
```
Ensure your request body is valid JSON. Check for trailing commas, unquoted keys, or mismatched brackets. Verify the `Content-Type: application/json` header is set.

### Rate Limit Errors

**Quota exceeded:**
```
HTTP 429
{"error": "Monthly request quota exceeded"}
```
Your tenant has used all allocated requests for the current billing period. Wait until the period resets or contact your administrator to upgrade your tier.

**Daily limit:**
```
HTTP 429
{"error": "Daily request limit reached for your tier"}
```
Your tenant has hit the daily rate cap. Wait until the next UTC day or upgrade your tier.

See [Rate Limits and Quotas](../platform/rate-limits.md) for details on limits by tier.

### Server Errors

**Internal server error:**
```
HTTP 500
{"error": "Internal server error"}
```
An unexpected error occurred on the server. These are not caused by your request. If the error persists, check service health via `GET /api/admin/services` or inspect server logs.

---

## Error Handling Best Practices

1. **Always check the HTTP status code first.** The status code tells you the error category before you parse the response body.

2. **Parse the error message for user display.** The `error` field is written to be human-readable and safe to show to end users.

3. **Retry on 429 and 500.** Rate limit errors (429) should be retried with exponential backoff. Server errors (500) may be transient — retry once or twice before treating as a permanent failure.

4. **Do not retry on 400, 401, 403, 404, 409, or 422.** These indicate problems with the request itself. Fix the request before retrying.

5. **Log the full response.** When debugging, log both the HTTP status code and the response body. The error message often contains the specific field or value that caused the problem.

### Example: Robust Error Handling (Python)

```python
import requests

def run_agent(api_key, agent_name, input_data):
    response = requests.post(
        f"https://api.ares.dirmacs.com/v1/agents/{agent_name}/run",
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
        },
        json={"input": input_data},
    )

    if response.status_code == 200:
        return response.json()

    error = response.json().get("error", "Unknown error")

    if response.status_code == 401:
        raise AuthenticationError(f"Authentication failed: {error}")
    elif response.status_code == 404:
        raise AgentNotFoundError(f"Agent '{agent_name}' not found: {error}")
    elif response.status_code == 429:
        raise RateLimitError(f"Rate limited: {error}")
    elif response.status_code >= 500:
        raise ServerError(f"Server error: {error}")
    else:
        raise APIError(f"API error ({response.status_code}): {error}")
```

### Example: Robust Error Handling (JavaScript)

```javascript
async function runAgent(apiKey, agentName, inputData) {
  const response = await fetch(
    `https://api.ares.dirmacs.com/v1/agents/${agentName}/run`,
    {
      method: "POST",
      headers: {
        "Authorization": `Bearer ${apiKey}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ input: inputData }),
    }
  );

  if (response.ok) {
    return await response.json();
  }

  const { error } = await response.json();

  switch (response.status) {
    case 401: throw new Error(`Authentication failed: ${error}`);
    case 404: throw new Error(`Agent '${agentName}' not found: ${error}`);
    case 429: throw new Error(`Rate limited: ${error}`);
    default:  throw new Error(`API error (${response.status}): ${error}`);
  }
}
```
