# Command Line Interface

The `ares-server` binary is the single entry point. Run it without a subcommand to start the server, or use a subcommand to manage a project.

```console
$ ares-server --help
A production-grade agentic chatbot server with multi-provider LLM support,
tool calling, RAG (Retrieval Augmented Generation), and MCP integration.

Run without arguments to start the server, or use 'init' to scaffold a new project.

Usage: ares-server [OPTIONS] [COMMAND]

Commands:
  init    Initialize a new A.R.E.S project with configuration files
  config  Show configuration information
  agent   Manage agents
  rag     Ingest and search RAG collections through the ARES API
  help    Print this message or the help of the given subcommand(s)
```

## Global options

These options work on every command:

| Option | Effect |
|---|---|
| `-c, --config <CONFIG>` | Path to the configuration file. Default: `ares.toml` |
| `-v, --verbose` | Enable verbose output |
| `--no-color` | Disable colored output |
| `--mcp` | Start in MCP server mode over stdio transport |
| `--supervise` | Run under the built-in supervisor |
| `-h, --help` | Print help. Use `-h` for a short form and `--help` for details |
| `-V, --version` | Print the version |

### Supervisor semantics

`--supervise` runs the real server in a child copy of the same executable. The child signals the parent through its exit code:

| Exit code | Meaning | Parent action |
|---|---|---|
| `51` | Hot-restart request | Respawn a fresh child |
| `52` | Clean shutdown | Stop the loop |
| `53` | Boot failure | Stop and mirror the non-zero code to the service manager |

Any other terminal status also ends the loop. Respawns that follow very short runs pace themselves with exponential backoff, from 100 ms up to a 5 s cap.

Example: run a daemon that survives hot restarts:

```bash
ares-server --supervise
```

Pair it with a service manager such as systemd. Boot failures still surface as non-zero exits.

## `init`

Scaffold a new project. Creates `ares.toml`, a `.env.example` template, the `data/` directory, and the `config/` directory tree with example TOON files.

```console
$ ares-server init [OPTIONS] [PATH]
```

| Option | Effect |
|---|---|
| `[PATH]` | Directory to initialize. Default: current directory |
| `-f, --force` | Overwrite existing files without prompting |
| `-m, --minimal` | Create fewer agents and tools |
| `--no-examples` | Skip example TOON files in `config/` |
| `--provider <PROVIDER>` | LLM provider to configure: `ollama`, `openai`, or `both`. Default: `ollama` |
| `--host <HOST>` | Server host address. Default: `127.0.0.1` |
| `--port <PORT>` | Server port. Default: `3000` |

Example session:

```console
$ ares-server init --minimal
   Agentic Runtime Extensible Server v0.10.0

  Initializing A.R.E.S Project

  Creating directories
  ✓ directory data
  ✓ directory config/agents
  ✓ directory config/models

  Creating configuration files
  ✓ config ares.toml
  ✓ env .env.example

  A.R.E.S project initialized successfully!
```

## `config`

Show configuration information.

```console
$ ares-server config [OPTIONS]
```

| Option | Effect |
|---|---|
| `-f, --full` | Show the full configuration instead of the summary |
| `--validate` | Validate the configuration file and report problems |

Example summary:

```console
$ ares-server config
  Configuration Summary

    Config file: ares.toml
    Server: 127.0.0.1:3000
    Log level: info

  Providers
    • ollama-local

  Agents
    • orchestrator
    • router
```

Validation fails when a referenced environment variable is missing, or when an agent references an unknown provider. A zero exit code means the file is valid:

```console
$ ares-server config --validate
  ✓ Configuration is valid!
```

## `agent`

Manage configured agents.

```console
$ ares-server agent <COMMAND>
```

Subcommands:

- `list` — list all configured agents.
- `show <NAME>` — show details for one agent.

Example listing:

```console
$ ares-server agent list
  Configured Agents

    Name            Model                        Tools
    ────────────────────────────────────────────────
    router          meta/llama-3.3-70b-instruct  -
    orchestrator    meta/llama-3.3-70b-instruct  calculator, web_search
```

Show one agent by name:

```bash
ares-server agent show orchestrator
```

Both commands read the same sources as the server: static agents from `ares.toml` plus TOON files in the configured directories.

## `rag`

Ingest documents into RAG collections and search them. These commands are API clients: they call a running ARES server over HTTP, so start the server first.

Authenticate with `--user` and `--password`, or skip login with `--token`. Both forms accept `--host` for a remote server. Default host: `http://localhost:3000`.

### `rag ingest-dir`

Recursively ingest local text documents into a collection.

```console
$ ares-server rag ingest-dir [OPTIONS] --collection <COLLECTION> --docs-path <DOCS_PATH>
```

Notable options:

| Option | Effect |
|---|---|
| `--collection <NAME>` | Collection to ingest into. Required |
| `--docs-path <DIR>` | Directory with the documents. Required |
| `--chunking-strategy <KIND>` | `word` (default), `semantic`, or `character` |
| `--tag <TAG>` | Attach a tag. Repeat for multiple tags |
| `--dry-run` | List the files that would be ingested. Send no requests |
| `--user` / `--password` | Login credentials |
| `--token <TOKEN>` | Bearer token; skips login |

Preview an ingest before running it:

```bash
ares-server rag ingest-dir \
  --collection docs \
  --docs-path ./handbook \
  --tag handbook \
  --dry-run
```

Remove `--dry-run` to perform the ingest.

### `rag search`

Search a collection.

```console
$ ares-server rag search [OPTIONS] --collection <COLLECTION> --query <QUERY>
```

Notable options:

| Option | Effect |
|---|---|
| `--collection <NAME>` | Collection to search. Required |
| `--query <TEXT>` | Search query. Required |
| `--top-k <N>` | Maximum number of results. Default: `10` |
| `--strategy <KIND>` | `semantic` (default), `bm25`, `fuzzy`, or `hybrid` |
| `--user` / `--password` | Login credentials |
| `--token <TOKEN>` | Bearer token; skips login |

Example:

```bash
ares-server rag search \
  --collection docs \
  --query "how do I rotate API keys" \
  --top-k 5 \
  --strategy hybrid
```

## Where to go next

- Scaffold and run your first project: [First Server](getting-started/first-server.md).
- Call the server over HTTP instead of the CLI: [HTTP API](http-api.md).
