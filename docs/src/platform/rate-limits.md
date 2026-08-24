# Rate limits and quotas

ARES enforces two independent layers of rate limiting. The layers protect the platform and give all tenants fair resource allocation.

---

## Layer 1: IP-Based rate limiting

Every incoming request passes through per-IP rate limiting via [tower_governor](https://docs.rs/tower_governor). This layer protects against abuse, brute-force attacks, and accidental request floods regardless of authentication status.

IP-based limits apply to all routes, including unauthenticated endpoints like `/health`. The server configuration sets the specific thresholds. Normal usage patterns stay well below them.

If you hit the IP rate limit, you receive a `429 Too Many Requests` response. Back off and retry after a short delay.

---

## Layer 2: tenant quotas

Authenticated requests to `/v1/*` also pass through tenant-level quotas based on the tier of the tenant. These quotas reset at the beginning of each calendar month.

| Tier | Monthly Requests | Monthly Tokens | Daily Rate Limit |
|---|---|---|---|
| **Free** | 1,000 | 100,000 | 100/day |
| **Dev** | 10,000 | 1,000,000 | 1,000/day |
| **Pro** | 100,000 | 10,000,000 | 10,000/day |
| **Enterprise** | Unlimited | Unlimited | Unlimited |

### What counts as a request

Each API call to a metered endpoint counts as one request:

- `POST /v1/agents/{name}/run`, 1 request
- `POST /v1/chat`, 1 request
- `POST /v1/chat/stream`, 1 request
- `GET /v1/agents`, 1 request

Read-only endpoints like `GET /v1/usage` and `GET /v1/api-keys` are metered but count toward the request total.

### What counts as tokens

Token usage is tracked per request from the combined input and output token count of the LLM provider. Prompt tokens and completion tokens are summed.

---

## Response headers

When you make a request to a metered endpoint, ARES includes rate limit information in the response headers:

| Header | Description |
|---|---|
| `X-RateLimit-Limit` | Maximum requests allowed in the current period |
| `X-RateLimit-Remaining` | Requests remaining in the current period |
| `X-RateLimit-Reset` | UTC timestamp when the current period resets |
| `X-Quota-Tokens-Remaining` | Tokens remaining in the current monthly period |

**Example headers:**

```
X-RateLimit-Limit: 10000
X-RateLimit-Remaining: 7482
X-RateLimit-Reset: 2026-04-01T00:00:00Z
X-Quota-Tokens-Remaining: 8241037
```

---

## Exceeding limits

When you exceed either rate limit layer, ARES returns:

```
HTTP/1.1 429 Too Many Requests
Content-Type: application/json

{
  "error": "Rate limit exceeded. Daily request limit reached for your tier."
}
```

The error message indicates which limit you hit:

| Error Message | Cause | Resolution |
|---|---|---|
| `Rate limit exceeded` | IP-based rate limit | Wait and retry. Reduce request frequency. |
| `Daily request limit reached for your tier` | Tenant daily cap | Wait until the next UTC day, or upgrade your tier. |
| `Monthly request quota exceeded` | Tenant monthly cap | Wait until the next billing period, or upgrade. |
| `Monthly token quota exceeded` | Tenant token cap | Wait until the next billing period, or upgrade. |

---

## Checking your usage

You can monitor your consumption before you hit limits:

```bash
curl http://localhost:3000/v1/usage \
  -H "Authorization: Bearer ares_xxx"
```

**Response:**

```json
{
  "period_start": "2026-03-01T00:00:00Z",
  "period_end": "2026-03-31T23:59:59Z",
  "total_runs": 4821,
  "total_tokens": 2847193,
  "total_api_calls": 5290,
  "quota_runs": 100000,
  "quota_tokens": 10000000,
  "daily_usage": [
    { "date": "2026-03-13", "runs": 312, "tokens": 184920, "api_calls": 340 }
  ]
}
```

Compare `total_runs` with `quota_runs` and `total_tokens` with `quota_tokens` to see how much headroom you have.

---

## Best practices

1. **Monitor usage proactively.** Poll `GET /v1/usage` periodically rather than wait for 429 errors.

2. **Implement exponential backoff.** When you receive a 429, wait before you retry. A simple strategy: wait 1s, then 2s, then 4s, up to a maximum of 30s.

3. **Cache where possible.** Agent listings and model metadata change infrequently. Cache these responses to reduce unnecessary API calls.

4. **Use streaming for chat.** `POST /v1/chat/stream` counts as a single request regardless of response length, same as the non-streaming variant.

5. **Request a tier upgrade early.** If you anticipate hitting your quota before month-end, contact your platform administrator to upgrade your tier. Tier changes take effect immediately.
