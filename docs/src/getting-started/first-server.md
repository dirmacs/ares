# First Server

This chapter runs your first ARES server. You scaffold a project, validate the configuration, start the server, and send one chat request.

The commands use `ares-server` from your `PATH`. Substitute your binary path if you built from source.

## Scaffold a project

Run `init` in an empty directory:

```bash
mkdir my-ares && cd my-ares
ares-server init --minimal
```

`--minimal` writes fewer agents and tools. Without it, `init` writes the full example set. Add `--no-examples` to skip example TOON files, or `--provider openai` to configure OpenAI instead of Ollama.

The command creates this layout:

```text
.
├── ares.toml          # Main configuration file
├── .env.example       # Template for environment variables
├── data/              # Local database and RAG data
└── config/
    ├── agents/        # TOON agent definitions
    ├── models/        # TOON model definitions
    ├── tools/         # TOON tool definitions
    ├── workflows/     # TOON workflow definitions
    └── mcps/          # TOON MCP server definitions
```

## Inspect the generated configuration

Open `ares.toml`. The minimal file contains these sections:

```toml
[server]
host = "127.0.0.1"
port = 3000
log_level = "info"

[auth]
jwt_secret_env = "JWT_SECRET"   # Name of the env var that holds the JWT secret
jwt_access_expiry = 900         # Access token lifetime in seconds
jwt_refresh_expiry = 604800     # Refresh token lifetime in seconds
api_key_env = "API_KEY"         # Name of the env var that holds the service API key

[database]
url = "./data/ares.db"          # Local SQLite-compatible store; or a postgres:// URL

[providers.ollama-local]
type = "ollama"
base_url = "http://localhost:11434"
default_model = "ministral-3:3b"

[agents.router]
model = "meta/llama-3.3-70b-instruct"
tools = []
max_tool_iterations = 1
system_prompt = "..."           # Multi-line prompt text

[config]
agents_dir = "config/agents"    # TOON directories, watched for hot reload
hot_reload = true
watch_interval_ms = 1000
```

Every credential is an environment-variable *name*, not a value. The server reads the value at startup.

## Set environment variables

Copy the template and fill in the values:

```bash
cp .env.example .env
```

Set at least these variables before you start:

```bash
export JWT_SECRET="$(openssl rand -base64 32)"   # At least 32 characters
export API_KEY="change-me-service-key"
```

With the default Ollama provider, also start Ollama and pull a model:

```bash
ollama serve
ollama pull ministral-3:3b
```

## Validate the configuration

Check the file before starting:

```bash
ares-server config --validate
```

The command reports valid configuration or names the problem:

```console
$ ares-server config --validate
  ✓ Configuration is valid!

  Configuration Summary

    Config file: ares.toml
    Server: 127.0.0.1:3000
    Log level: info
```

Validation fails when a named environment variable is missing, or when an agent references an unknown provider.

## Start the server

Run without arguments to start:

```bash
ares-server
```

The server binds to `127.0.0.1:3000`. It watches `ares.toml` and the TOON directories. Edits apply while it runs.

## Send a chat request

The external chat route is `POST /v1/chat`. It authenticates with a tenant API key of the form `ares_...`.

Create a tenant and its first key. The admin routes need the `ADMIN_API_KEY` environment variable on the server process and the matching `x-admin-secret` header:

```bash
# Start the server with admin routes enabled
ADMIN_API_KEY="local-admin-secret" ares-server &

# Create a tenant
curl -s -X POST http://localhost:3000/admin/tenants \
  -H "x-admin-secret: $ADMIN_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"name": "local", "tier": "free"}'

# Create an API key for that tenant (use the tenant id from the response)
curl -s -X POST http://localhost:3000/admin/tenants/TENANT_ID/api-keys \
  -H "x-admin-secret: $ADMIN_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"name": "first-key"}'
```

The second response carries `raw_key`. Store it now; you cannot read it again.

Export the raw key for later requests:

```bash
export ARES_TENANT_API_KEY="ares_paste-the-raw-key-here"
```

Send the chat request with that key:

```bash
curl -s -X POST http://localhost:3000/v1/chat \
  -H "Authorization: Bearer $ARES_TENANT_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"message": "What is 21 times 2?"}'
```

The response echoes the answering agent and the token usage:

```json
{
  "response": "42",
  "agent": "orchestrator",
  "source": "registry",
  "model": "ministral-3:3b",
  "provider": "ollama-local",
  "usage": {
    "input_tokens": 12,
    "output_tokens": 3
  }
}
```

## Stop the server

Press `Ctrl+C` in the server terminal. The server shuts down cleanly.

For daemon-style operation, run under the built-in supervisor:

```bash
ares-server --supervise
```

The supervisor respawns the child after a hot-restart exit (code 51) and stops on clean exits. See [Command Line Interface](../cli.md) for details.
