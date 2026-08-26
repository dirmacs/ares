# First Server

This chapter runs your first ARES server. You scaffold a project, validate the configuration, start the server, and send one chat request.

The commands use `ares-server` from your `PATH`. Substitute your binary path if you built from source.

## Scaffold a project

Run `init` in an empty directory:

```bash
mkdir my-ares && cd my-ares
ares-server init
```

`init` accepts a target path, `--force` to overwrite, and these switches:

- `--no-examples` skips the example TOON files under `config/`. Verified against 0.10.0: the run then creates only `ares.toml`, `.env.example`, `.gitignore`, and empty directories.
- `--provider openai|both` selects which provider template lands in `ares.toml`; the default is `ollama`.
- `--minimal` asks for a smaller configuration. In 0.10.0 both modes write the same file set; prefer plain `init`.

The command creates this layout:

```text
.
├── ares.toml          # Main configuration file
├── .env.example       # Template for environment variables
├── .gitignore
├── data/              # Local database and RAG data
└── config/
    ├── agents/        # router.toon, orchestrator.toon
    ├── models/        # fast.toon, balanced.toon, powerful.toon
    ├── tools/         # calculator.toon, web_search.toon
    ├── workflows/     # default.toon, research.toon
    └── mcps/          # TOON MCP server definitions (empty)
```

## Inspect the generated configuration

Open `ares.toml`. The generated file contains these sections (comments stripped):

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

[tools.calculator]
enabled = true
description = "Performs basic arithmetic operations (+, -, *, /)"
timeout_secs = 10

[agents.router]
model = "meta/llama-3.3-70b-instruct"
tools = []
max_tool_iterations = 1
parallel_tools = false
system_prompt = """..."""        # Routing prompt; answers with one agent name

[agents.orchestrator]
model = "meta/llama-3.3-70b-instruct"
tools = ["calculator", "web_search"]
max_tool_iterations = 10

[workflows.default]
entry_agent = "router"
fallback_agent = "orchestrator"
max_depth = 3

[config]
agents_dir = "config/agents"    # TOON directories, watched for hot reload
hot_reload = true
watch_interval_ms = 1000
```

A `[rag]` block also appears (`embedding_model = "BAAI/bge-small-en-v1.5"`, chunk size and overlap). Retrieval reads it when you add collections.

### What each block does

**`[server]`** controls the listener and logging. `host` and `port` feed the single `TcpListener::bind` call in `run_server` (`src/main.rs`). Keep `127.0.0.1` while you test; use `0.0.0.0` only when another machine must connect. Two more fields exist beyond the template: `cors_origins` (default `["http://localhost:3000"]`, set explicit origins in production) and the rate-limit pair below.

```toml
rate_limit_per_second = 100   # Requests per second per IP; 0 disables limiting
rate_limit_burst = 10         # Bucket size admitted above the steady rate
```

The limiter is `tower_governor` (`src/main.rs`, rate-limit layer build): it admits bursts up to `rate_limit_burst` and refills one slot every $1/\text{rate\_limit\_per\_second}$ seconds. Responses carry `x-ratelimit-*` headers.

**`[auth]`** names the secret environment variables and token lifetimes. `jwt_secret_env = "JWT_SECRET"` means: read the signing key from the environment variable called `JWT_SECRET`. The same pattern applies to `api_key_env`. Lifetimes are seconds: 900 gives 15-minute access tokens; 604800 gives 7-day refresh tokens.

**`[database]`** holds one connection URL. The template writes a local file path for an embedded store. A production deployment points at `postgres://user:pass@host/db`; the store then runs embedded migrations from `ares_store::MIGRATOR` on first connect.

**`[providers.<name>]`** defines one LLM provider. `<name>` (`ollama-local` here) is your label for it; agents reference this label. `type` picks the client implementation. `base_url` is where the client sends requests. `default_model` applies when an agent names no model of its own.

**`[tools.<name>]`** declares one tool in the main file: an `enabled` switch, a description the model sees, and a `timeout_secs` cap. The matching TOON file under `config/tools/` carries the same fields for hot reload.

**`[agents.<name>]`** defines one agent. `model` selects the model (a provider default when omitted). `tools` lists callable tools; empty means the agent answers without side effects. `max_tool_iterations` caps the tool-call loop rounds. `parallel_tools` runs independent calls in one round together. `system_prompt` sets the persona text sent with every request. The scaffold defines two agents: `router` classifies a request and answers with one agent name; `orchestrator` does the real work and owns the tools.

**`[workflows.default]`** chains agents. A workflow enters at `entry_agent` and falls back to `fallback_agent`; `max_depth` bounds the hops.

**`[config]`** points at the TOON directories. With `hot_reload = true`, a watcher re-reads them every `watch_interval_ms`. Edits apply without a restart.

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

Validation checks structure and cross-references: unknown providers in agent blocks and malformed TOML fail here. Verified against 0.10.0: validation passes even while `JWT_SECRET` and `API_KEY` are unset, so export them anyway before you start.

## Start the server

Run without arguments to start:

```bash
ares-server
```

### What happens on boot

`run_server` in `src/main.rs` runs one ordered pass. Each step gates the next:

1. **Load environment and tracing** (`src/main.rs:528-532`). `.env` is read if present, then the log filter starts at `info`.
2. **Create the root context** (`src/main.rs:534-554`). A Cordis root `Context` appears, plus a `ReflectService`. The service registers notifiers for the `Tools` and `Llm` types so later changes fan out immediately.
3. **Register loader factories** (`src/main.rs:557-558`). Built-in factories (Store, Llm, Tools, CalculatorService, and others) enter the `PluginRegistry`, either through explicit chains or inventory collection.
4. **Boot the entries program** (`src/main.rs:560-568`). The loader parses `config/cordis-entries.toml` and instantiates entries in file order. When it reaches the `Overlay` entry, empty entry configs fill from `ares.toml`. The `Store` factory connects to the database, runs migrations, and seeds default agents here. Any boot failure logs `Cordis Loader: boot failed` and exits with code 1.
5. **Guard configuration presence** (`src/main.rs:570-585`). No loaded config means a friendly error that points at `ares-server init`, then exit 1.
6. **Start the entries watcher** (`src/main.rs:592-625`). File events re-compose the program and apply diffs through the loader journal. If the watcher cannot start, a 30-second poll takes over.
7. **Preload runtime providers and snapshot agent configs** (`src/main.rs:630-707`). Runtime provider registrations load; current agent definitions land in the version history table.
8. **Build HTTP layers** (`src/main.rs:895-944`). CORS applies from `cors_origins`; the rate-limit layer builds only when `rate_limit_per_second > 0`.
9. **Bind and serve** (`src/main.rs:949-964`). The listener binds `host:port`, and Axum serves with graceful shutdown on Ctrl+C or SIGTERM.

If step 4 or 9 fails, you see the reason in the log before the process exits. Nothing listens before step 9, so a failure never leaves a half-open port.

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

## Exercise a tool through chat

The scaffold already wires one tool-capable agent: `orchestrator` lists `calculator` and `web_search` in its `tools` field, and both tool files exist under `config/tools/`. The `router` agent carries `tools = []` on purpose.

To exercise the calculator directly, ask through the orchestrator path:

```bash
curl -s -X POST http://localhost:3000/v1/chat \
  -H "Authorization: Bearer $ARES_TENANT_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"message": "Use the calculator to compute 21 * 2."}'
```

What happens inside:

1. `Execute::run` receives the message.
2. The model answers with a tool call instead of text.
3. ARES resolves `calculator` through the `Tools` service and executes it.
4. ARES sends the tool result back to the model, up to `max_tool_iterations` rounds.
5. The final response arrives in the same JSON shape as before. Token usage now covers every round.

You can also give the `router` agent tools: set `tools = ["calculator"]` in `ares.toml`, or add `tools[0]: calculator` to `config/agents/router.toon`. Save the file; hot reload applies it within about one second.

If the response says the tool is unknown, check three things: the tool file exists under `config/tools/`, `enabled: true` is set, and the agent's `tools` list matches the tool name exactly.

## Common first-run errors

| Symptom | Cause | Fix |
|---|---|---|
| Bind error such as `Address already in use (os error 98)` | Another process holds `host:port`; often a previous server that never stopped | Stop the old process, or change `port` in `[server]` |
| Chat returns an authentication error | Missing, malformed, or revoked tenant API key | Confirm the header reads `Bearer ares_...` with the raw key from creation time |
| `Cordis Loader: boot failed` in the log, then exit code 1 | A loader entry failed during boot; most often the database URL points at an unreachable server | Check `[database].url`; for `postgres://` URLs confirm the server accepts connections, then retry |
| Friendly banner naming the missing config file, exit code 1 | `ares.toml` absent from the working directory | Run `ares-server init` in that directory, or start from a directory that has one |
| Chat fails with a provider connection error | Ollama (or your provider) is down, or `base_url` is wrong | Start `ollama serve`, pull the model named in the provider block, and re-check `base_url` |
| Admin routes answer 401 | Server started without `ADMIN_API_KEY` set | Restart the server process with `ADMIN_API_KEY` exported |

## Stop the server

Press `Ctrl+C` in the server terminal. The server shuts down cleanly.

For daemon-style operation, run under the built-in supervisor:

```bash
ares-server --supervise
```

The supervisor respawns the child after a hot-restart exit (code 51) and stops on clean exits. See [Command Line Interface](../cli.md) for details.
