# Authentication

ARES supports three authentication methods, each designed for a different use case.

| Method | Header | Routes | Use case |
|---|---|---|---|
| API Key | `Authorization: Bearer ares_xxx` | `/v1/*` | Client applications, backend services |
| JWT | `Authorization: Bearer <access_token>` | `/api/*` | End-user sessions, frontend apps |
| Admin Secret | `X-Admin-Secret: <secret>` | `/api/admin/*` | Internal administration |

---

## API Key authentication

API keys are the simplest way to authenticate with ARES. Each key is scoped to a single tenant and carries that tenant's permissions and rate limits.

**Format:** `ares_` followed by a random string (e.g., `ares_k7Gx9mPqR2vLwN4s`).

**How to get one:** API keys are generated during tenant provisioning via the [Dirmacs Admin](https://admin.dirmacs.com) dashboard, or through the admin API.

### Usage

Pass the API key in the `Authorization` header on any `/v1/*` endpoint:

```bash
curl -X POST https://api.ares.dirmacs.com/v1/chat \
  -H "Authorization: Bearer ares_k7Gx9mPqR2vLwN4s" \
  -H "Content-Type: application/json" \
  -d '{"message": "Hello", "agent_type": "product"}'
```

```python
import requests

headers = {
    "Authorization": "Bearer ares_k7Gx9mPqR2vLwN4s",
    "Content-Type": "application/json",
}

response = requests.post(
    "https://api.ares.dirmacs.com/v1/chat",
    headers=headers,
    json={"message": "Hello", "agent_type": "product"},
)
```

```javascript
const response = await fetch("https://api.ares.dirmacs.com/v1/chat", {
  method: "POST",
  headers: {
    "Authorization": "Bearer ares_k7Gx9mPqR2vLwN4s",
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ message: "Hello", agent_type: "product" }),
});
```

> **Security:** Treat API keys like passwords. Do not embed them in client-side code, commit them to version control, or expose them in logs. Use environment variables or a secrets manager.

---

## JWT authentication

JWT authentication is designed for end-user sessions. Users register and log in to receive short-lived access tokens and long-lived refresh tokens.

- **Access tokens** expire after 15 minutes.
- **Refresh tokens** are used to obtain new access tokens without re-entering credentials.

### Register a new user

```bash
curl -X POST https://api.ares.dirmacs.com/api/auth/register \
  -H "Content-Type: application/json" \
  -d '{
    "email": "developer@example.com",
    "password": "your-secure-password",
    "name": "Jane Developer"
  }'
```

**Response:**

```json
{
  "message": "Registration successful",
  "user_id": "usr_abc123"
}
```

### Log in

```bash
curl -X POST https://api.ares.dirmacs.com/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{
    "email": "developer@example.com",
    "password": "your-secure-password"
  }'
```

**Response:**

```json
{
  "access_token": "eyJhbGciOiJIUzI1NiIs...",
  "refresh_token": "rt_x9Kp2mQvL8wN3rTs...",
  "expires_in": 900
}
```

### Use the access token

Pass the access token in the `Authorization` header on any `/api/*` endpoint:

```bash
curl https://api.ares.dirmacs.com/api/chat \
  -H "Authorization: Bearer eyJhbGciOiJIUzI1NiIs..." \
  -H "Content-Type: application/json" \
  -d '{"message": "Hello", "agent_type": "product"}'
```

### Refresh an expired token

When your access token expires, use the refresh token to get a new one:

```bash
curl -X POST https://api.ares.dirmacs.com/api/auth/refresh \
  -H "Content-Type: application/json" \
  -d '{
    "refresh_token": "rt_x9Kp2mQvL8wN3rTs..."
  }'
```

**Response:**

```json
{
  "access_token": "eyJhbGciOiJIUzI1NiIs...",
  "expires_in": 900
}
```

### Log out

Invalidate a refresh token when the user logs out:

```bash
curl -X POST https://api.ares.dirmacs.com/api/auth/logout \
  -H "Content-Type: application/json" \
  -d '{
    "refresh_token": "rt_x9Kp2mQvL8wN3rTs..."
  }'
```

### Token management in Python

```python
import requests
import time


class AresClient:
    def __init__(self, base_url="https://api.ares.dirmacs.com"):
        self.base_url = base_url
        self.access_token = None
        self.refresh_token = None
        self.token_expiry = 0

    def login(self, email, password):
        response = requests.post(
            f"{self.base_url}/api/auth/login",
            json={"email": email, "password": password},
        )
        data = response.json()
        self.access_token = data["access_token"]
        self.refresh_token = data["refresh_token"]
        self.token_expiry = time.time() + data["expires_in"]

    def _ensure_valid_token(self):
        if time.time() >= self.token_expiry - 30:  # Refresh 30s before expiry
            response = requests.post(
                f"{self.base_url}/api/auth/refresh",
                json={"refresh_token": self.refresh_token},
            )
            data = response.json()
            self.access_token = data["access_token"]
            self.token_expiry = time.time() + data["expires_in"]

    def chat(self, message, agent_type="product"):
        self._ensure_valid_token()
        response = requests.post(
            f"{self.base_url}/api/chat",
            headers={"Authorization": f"Bearer {self.access_token}"},
            json={"message": message, "agent_type": agent_type},
        )
        return response.json()
```

### Token management in JavaScript

```javascript
class AresClient {
  constructor(baseUrl = "https://api.ares.dirmacs.com") {
    this.baseUrl = baseUrl;
    this.accessToken = null;
    this.refreshToken = null;
    this.tokenExpiry = 0;
  }

  async login(email, password) {
    const response = await fetch(`${this.baseUrl}/api/auth/login`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ email, password }),
    });
    const data = await response.json();
    this.accessToken = data.access_token;
    this.refreshToken = data.refresh_token;
    this.tokenExpiry = Date.now() + data.expires_in * 1000;
  }

  async ensureValidToken() {
    if (Date.now() >= this.tokenExpiry - 30000) {
      const response = await fetch(`${this.baseUrl}/api/auth/refresh`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ refresh_token: this.refreshToken }),
      });
      const data = await response.json();
      this.accessToken = data.access_token;
      this.tokenExpiry = Date.now() + data.expires_in * 1000;
    }
  }

  async chat(message, agentType = "product") {
    await this.ensureValidToken();
    const response = await fetch(`${this.baseUrl}/api/chat`, {
      method: "POST",
      headers: {
        "Authorization": `Bearer ${this.accessToken}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ message, agent_type: agentType }),
    });
    return response.json();
  }
}
```

---

## Admin Secret authentication

The admin secret provides full access to ARES administration endpoints. It is intended for internal tools and the Dirmacs Admin dashboard only.

Pass the secret in the `X-Admin-Secret` header:

```bash
curl https://api.ares.dirmacs.com/api/admin/tenants \
  -H "X-Admin-Secret: your-admin-secret"
```

> **Warning:** The admin secret grants unrestricted access to all tenants, agents, and configuration. Never expose it outside your infrastructure. It should only be used in server-to-server calls from trusted internal services.

---

## Error responses

Authentication failures return standard HTTP status codes:

| Status | Meaning |
|---|---|
| `401 Unauthorized` | Missing or invalid credentials |
| `403 Forbidden` | Valid credentials but insufficient permissions |
| `429 Too Many Requests` | Rate limit exceeded for this API key or tenant |

Example error response:

```json
{
  "error": "Invalid or expired token",
  "code": "AUTH_INVALID_TOKEN"
}
```
