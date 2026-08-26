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

### Global flag placement

Every option carries `global = true` (`src/cli/mod.rs`). Place a global flag before or after the subcommand. These two lines parse identically:

```bash
ares-server --no-color config --validate
ares-server config --validate --no-color
```

`-v, --verbose` changes the log filter of a server run from `info` to `debug,ares=trace` (`run_server`, `src/main.rs`). Subcommands print their diagnostics to standard error regardless of this flag.

`--mcp` needs the `mcp` build feature. Without it, the binary prints a rebuild hint and exits `1`.

### Supervisor semantics

`--supervise` runs the real server in a child copy of the same executable. The child signals the parent through its exit code:

| Exit code | Meaning | Parent action |
|---|---|---|
| `51` | Hot-restart request | Respawn a fresh child |
| `52` | Clean shutdown | Stop the loop |
| `53` | Boot failure | Stop and mirror the non-zero code to the service manager |

The constants live in `src/supervisor.rs`. The child carries the `CORDIS_SUPERVISED` environment variable. The daemon holds the write end of the child's standard input. Dropping it closes the pipe, so the child sees end-of-file and tears down gracefully.

Example: run a daemon that survives hot restarts:

```bash
ares-server --supervise
```

Pair it with a service manager such as systemd. Boot failures still surface as non-zero exits.

### Restart safeguards

Four safeguards keep the loop responsive (`src/supervisor.rs`):

- **Rapid-restart guard.** Five exits inside any 30-second window stop the loop. This bounds a crash loop that a misbehaving child cannot out-wait.
- **Health ladder reset.** A child that ran for at least 10 minutes before exiting counts as healthy. The next crash sequence starts its backoff from zero, not from stale strikes.
- **Exponential backoff.** A child that exited within 10 seconds never proved it could serve. The daemon delays the respawn: 100 ms first, then double each consecutive unhealthy run, capped at 5 s.
- **Stop grace.** A stopped worker gets 10 seconds to exit after its standard input closes. A worker that ignores the request is force-killed.

### Exit codes

Use these codes in scripts and service units:

| Code | Producer | Meaning |
|---|---|---|
| `0` | every subcommand | Success. `config --validate` reports a valid file |
| `1` | `init` | Target files already exist (`--force` overwrites), or scaffolding failed |
| `1` | server boot | Missing config file, missing Overlay entry, failed entries program, or `--mcp` without the feature |
| `51` | supervised child | Hot-restart request; the daemon respawns |
| `52` | supervised child | Clean shutdown; the daemon stops |
| `53` | supervised child | Boot failure; the daemon stops and mirrors `53` |

Two details matter for wrappers:

- Without `--supervise`, a failing boot returns `1`. With `--supervise`, the parent mirrors the real child code (`exit(last_code & 0xff)`), so a boot failure surfaces as `53`.

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

All options (`--help` output):

| Option | Effect |
|---|---|
| `--collection <NAME>` | Collection to ingest into. Required |
| `--docs-path <DIR>` | Directory with the documents. Required |
| `--chunking-strategy <KIND>` | `word` (default), `semantic`, or `character` |
| `--tag <TAG>` | Attach a tag. Repeat for multiple tags |
| `--dry-run` | List the files that would be ingested. Send no requests |
| `--host <URL>` | ARES server base URL. Default: `http://localhost:3000` |
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

All options (`--help` output):

| Option | Effect |
|---|---|
| `--collection <NAME>` | Collection to search. Required |
| `--query <TEXT>` | Search query. Required |
| `--top-k <N>` | Maximum number of results. Default: `10` |
| `--strategy <KIND>` | `semantic` (default), `bm25`, `fuzzy`, or `hybrid` |
| `--host <URL>` | ARES server base URL. Default: `http://localhost:3000` |
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

## Scripting and composition

Every command reports its result through the exit code. Chain commands with `&&` so a later step runs only after an earlier step succeeds.

Validate, then start under the supervisor:

```bash
ares-server config --validate && ares-server --supervise
```

Preview an ingest, then run it for real:

```bash
ares-server rag ingest-dir --collection docs --docs-path ./handbook --dry-run \
  && ares-server rag ingest-dir --collection docs --docs-path ./handbook \
       --tag handbook --user me@example.com --password "$PASS"
```

Scripting guidance:

- Check `$?` after each call. `0` means success; see the exit-code table above for failure codes.
- `--verbose` raises server log verbosity to `debug,ares=trace`. It does not change subcommand output.
- Progress lines go to standard output; failures go to standard error. Redirect them separately: `2>err.log`.
- A failed ingest still processes every document first. The summary line on stdout reads `summary<TAB>documents=N succeeded=S failed=F chunks=C`; parse it to decide whether to retry.
- A dry run prints one tab-separated line per file: `<path><TAB><title><TAB><N bytes>`, then `dry_run=true documents=<count>`. Both formats are stable for parsing.

A guarded ingest in shell:

```bash
if ares-server rag ingest-dir --collection docs --docs-path ./handbook \
     --token "$ARES_TOKEN" > out.log 2> err.log; then
  grep '^summary' out.log
else
  cat err.log
  exit 1
fi
```

## Where to go next

- Scaffold and run your first project: [First Server](getting-started/first-server.md).
- Call the server over HTTP instead of the CLI: [HTTP API](http-api.md).
