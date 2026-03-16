# Deployment API

The Deployment API allows you to trigger, monitor, and inspect deployments of ARES platform services. Deployments run server-side on the VPS and stream build output for observability.

**Base URL:** `https://api.ares.dirmacs.com`

## Authentication

All deployment endpoints require the admin secret:

```
X-Admin-Secret: <secret>
```

---

## Trigger a Deployment

```
POST /api/admin/deploy
```

Starts a deployment for the specified target service. The deployment runs asynchronously — you receive a deployment ID immediately and poll for completion.

**Request Body:**

```json
{
  "target": "ares"
}
```

| Target | Description |
|---|---|
| `ares` | ARES backend — pulls latest code, rebuilds, and restarts |
| `admin` | dirmacs-admin dashboard — rebuilds Leptos frontend |
| `eruka` | Eruka backend — pulls, rebuilds, and restarts |

**Response:**

```json
{
  "id": "deploy-uuid",
  "status": "running",
  "message": "Deployment started for ares"
}
```

**curl Example:**

```bash
curl -X POST https://api.ares.dirmacs.com/api/admin/deploy \
  -H "X-Admin-Secret: your-admin-secret" \
  -H "Content-Type: application/json" \
  -d '{"target": "ares"}'
```

---

## Poll Deployment Status

```
GET /api/admin/deploy/{id}
```

Returns the current status of a deployment. Poll this endpoint until `status` is no longer `"running"`.

**Response:**

```json
{
  "id": "deploy-uuid",
  "target": "ares",
  "status": "success",
  "started_at": "2026-03-13T14:00:00Z",
  "finished_at": "2026-03-13T14:03:42Z",
  "output": "Pulling latest changes...\nCompiling ares-server v0.1.0...\nFinished release target(s) in 3m 41s\nRestarting ares.service...\nService started successfully."
}
```

**Status Values:**

| Status | Meaning |
|---|---|
| `running` | Deployment is in progress |
| `success` | Deployment completed successfully |
| `failed` | Deployment failed — check `output` for details |

### Polling Pattern

The recommended approach is to trigger a deployment, then poll every 3 seconds until it completes:

```bash
# 1. Trigger deployment
DEPLOY_ID=$(curl -s -X POST https://api.ares.dirmacs.com/api/admin/deploy \
  -H "X-Admin-Secret: your-admin-secret" \
  -H "Content-Type: application/json" \
  -d '{"target": "ares"}' | jq -r '.id')

echo "Deployment started: $DEPLOY_ID"

# 2. Poll until complete
while true; do
  RESULT=$(curl -s https://api.ares.dirmacs.com/api/admin/deploy/$DEPLOY_ID \
    -H "X-Admin-Secret: your-admin-secret")

  STATUS=$(echo "$RESULT" | jq -r '.status')
  echo "Status: $STATUS"

  if [ "$STATUS" != "running" ]; then
    echo "$RESULT" | jq -r '.output'
    break
  fi

  sleep 3
done
```

**Python Example:**

```python
import requests
import time

ADMIN_SECRET = "your-admin-secret"
BASE_URL = "https://api.ares.dirmacs.com"
headers = {
    "X-Admin-Secret": ADMIN_SECRET,
    "Content-Type": "application/json",
}

# Trigger
resp = requests.post(
    f"{BASE_URL}/api/admin/deploy",
    headers=headers,
    json={"target": "ares"},
)
deploy_id = resp.json()["id"]
print(f"Deployment started: {deploy_id}")

# Poll
while True:
    resp = requests.get(
        f"{BASE_URL}/api/admin/deploy/{deploy_id}",
        headers=headers,
    )
    result = resp.json()
    print(f"Status: {result['status']}")

    if result["status"] != "running":
        print(result["output"])
        break

    time.sleep(3)
```

---

## List Recent Deployments

```
GET /api/admin/deploys
```

Returns the 20 most recent deployments, newest first.

**Response:**

```json
{
  "deploys": [
    {
      "id": "deploy-uuid",
      "target": "ares",
      "status": "success",
      "started_at": "2026-03-13T14:00:00Z",
      "finished_at": "2026-03-13T14:03:42Z"
    },
    {
      "id": "deploy-uuid-2",
      "target": "admin",
      "status": "failed",
      "started_at": "2026-03-12T10:00:00Z",
      "finished_at": "2026-03-12T10:02:15Z"
    }
  ]
}
```

**curl Example:**

```bash
curl https://api.ares.dirmacs.com/api/admin/deploys \
  -H "X-Admin-Secret: your-admin-secret"
```

---

## Service Health

### List All Services

```
GET /api/admin/services
```

Returns the runtime status of all managed services.

**Response:**

```json
{
  "ares": {
    "status": "running",
    "pid": 12847,
    "port": 3000
  },
  "eruka": {
    "status": "running",
    "pid": 12901,
    "port": 8081
  },
  "admin": {
    "status": "running",
    "pid": null,
    "port": null
  }
}
```

| Status | Meaning |
|---|---|
| `running` | Service is up and healthy |
| `stopped` | Service is not running |
| `degraded` | Service is running but unhealthy |

**curl Example:**

```bash
curl https://api.ares.dirmacs.com/api/admin/services \
  -H "X-Admin-Secret: your-admin-secret"
```

### Get Service Logs

```
GET /api/admin/services/{name}/logs
```

Returns recent log output from the service's systemd journal.

**Response:**

```json
{
  "service": "ares",
  "lines": [
    "Mar 13 14:03:42 vps ares-server[12847]: Listening on 0.0.0.0:3000",
    "Mar 13 14:03:42 vps ares-server[12847]: Connected to PostgreSQL",
    "Mar 13 14:03:43 vps ares-server[12847]: Loaded 29 agents, 4 providers, 11 models",
    "Mar 13 14:04:01 vps ares-server[12847]: POST /v1/agents/risk-analyzer/run 200 1243ms"
  ]
}
```

**curl Example:**

```bash
curl https://api.ares.dirmacs.com/api/admin/services/ares/logs \
  -H "X-Admin-Secret: your-admin-secret"
```
