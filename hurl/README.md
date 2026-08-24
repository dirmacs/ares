# Hurl API tests for A.R.E.S

These `.hurl` files exercise the running A.R.E.S server end-to-end.

## Prerequisites

- The `ares` server runs (default: `http://127.0.0.1:3000`)
- `hurl` is installed and available on PATH
- `just` (recommended) is a command runner that simplifies test execution

## Run the suite

### Using just (Recommended)

```bash
# Run all tests
just hurl

# Run with verbose output
just hurl-verbose

# Run specific test groups
just hurl-health      # Health check only
just hurl-auth        # Authentication tests
just hurl-chat        # Chat tests
just hurl-research    # Research tests

# Run a specific file
just hurl-file hurl/cases/00_health.hurl
```

### Using Nu shell (Alternative)

```nu
./scripts/hurl/run.nu
```

## Configure

Override the defaults through environment variables:

- `ARES_BASE_URL` (default `http://127.0.0.1:3000`)
- `ARES_TEST_EMAIL` / `ARES_TEST_PASSWORD` / `ARES_TEST_NAME`

Example:

```bash
# With just
ARES_BASE_URL=http://192.168.1.100:3000 just hurl

# With Nu shell
$env.ARES_BASE_URL = "http://127.0.0.1:3000"
$env.ARES_TEST_EMAIL = "hurl.user1@example.com"
$env.ARES_TEST_PASSWORD = "correcthorsebatterystaple"
./scripts/hurl/run.nu
```

## Notes

- `hurl/cases/10_auth_register_login_refresh.hurl` tolerates re-runs: the register step can return `400` if the user already exists, and the test still proceeds to login.
- `hurl/cases/21_research.hurl` allows `HTTP 200|500` because research depends on a configured and available LLM. With Ollama running, the endpoint returns 200.
