# Installation

This chapter installs the `ares-server` binary. Pick one method:

- Install from crates.io with `cargo install`.
- Build from source with `cargo build`.

## Prerequisites

You need a Rust toolchain. The crate declares `rust-version = "1.98"`, so use Rust 1.98 or newer. Check your version:

```console
$ rustc --version
```

Install Rust through [rustup](https://rustup.rs) if you do not have it.

## Install from crates.io

The crate is published as [`ares-server`](https://crates.io/crates/ares-server). Install it without pinning a version:

```bash
cargo install ares-server
```

To include the embedded web UI in the build, add the `ui` feature:

```bash
cargo install ares-server --features ui
```

The install puts the binary in `$HOME/.cargo/bin`. Make sure that directory is on your `PATH`.

## Build from source

Clone the repository and build the release binary:

```bash
git clone https://github.com/dirmacs/ares
cd ares
cargo build --release
```

The binary lands at `target/release/ares-server`. Copy it to a directory on your `PATH`, or call it by path.

## Feature flags

Features select LLM providers, database backends, and vector stores. The table lists every feature of the `ares-server` package.

| Feature | What it enables |
|---|---|
| `default` | `postgres`, `openai`, `ares-vector`, `mcp`, `inventory`, `rhai-policy` |
| `openai` | OpenAI API and compatible endpoints such as NVIDIA NIM |
| `azure` | Azure AI Foundry chat completions |
| `bedrock` | Claude on AWS Bedrock |
| `postgres` | PostgreSQL tenant database through `sqlx` (default) |
| `turso` | Turso/libSQL, an edge-native SQLite-compatible store |
| `ares-vector` | Embedded pure-Rust vector store with HNSW (default) |
| `lancedb` | LanceDB embedded vector store; needs `protoc` |
| `qdrant` | Qdrant vector database client |
| `pgvector` | pgvector, a PostgreSQL extension for vectors |
| `chromadb` | ChromaDB vector database client |
| `pinecone` | Pinecone managed vector database (alpha) |
| `mcp` | MCP protocol glue, client, auth, and registry |
| `inventory` | Cordis static registration at compile time (default) |
| `rhai-policy` | Rhai policy scripts on kernel events (default) |
| `eruka-context` | Per-agent context injection from Eruka |
| `local-embeddings` | ONNX local embedding models; not on Windows MSVC |
| `hmr` | Hot swap of compiled plugins through `dlopen` |
| `skills` | SKILL.md discovery and loading |
| `email` | Email sending over SMTP |
| `search-tools` | Web search and scraping tools |
| `ui` | Embedded Leptos web UI served by the backend |
| `swagger-ui` | Interactive API documentation pages |

Feature bundles combine several flags:

| Bundle | Contents |
|---|---|
| `all-llm` | `openai`, `azure`, `bedrock` |
| `all-db` | `postgres` |
| `all-vectorstores` | `ares-vector`, `qdrant`, `pgvector`, `chromadb`, `pinecone` |
| `local-vectorstores` | `ares-vector` only |
| `full` | All LLM providers, `postgres`, `qdrant`, `ares-vector`, `mcp`, `swagger-ui` |
| `full-ui` | `full` plus `ui` |
| `minimal` | Nothing optional |

### Choose feature combinations

Features compose along three independent axes. Pick one option per axis:

1. **LLM providers** (`openai`, `azure`, `bedrock`, or none for Ollama). These add provider clients to `ares-llm`. They do not interact with each other, so `all-llm` is safe when you want runtime choice.
2. **Database backend** (`postgres` or `turso`). The server binary requires the `postgres` feature. A binary built without it prints a rebuild hint and exits with code 1 at startup (`src/main.rs` compiles a stub `main` without it). Keep `postgres` unless you embed the library and run no HTTP server.
3. **Vector store** (`ares-vector`, `qdrant`, `pgvector`, `chromadb`, `pinecone`, `lancedb`). Clients are additive. `local-vectorstores` keeps the build small because only the embedded store compiles.

Cross-axis rules worth knowing:

- `postgres` also gates sqlx code paths in `ares-store`, `ares-agent`, `ares-mcp`, `ares-tools`, and `ares-http` through feature forwarding.
- `mcp`, `inventory`, and `rhai-policy` ride in `default`; dropping `default` drops all three. Re-add them explicitly if you build with `--no-default-features` plus your own picks.
- `swagger-ui` needs nothing extra, but the OpenAPI document includes RAG paths only when both `local-embeddings` and `ares-vector` are on (see the `#[cfg(all(...))]` gate around the `OpenApi` derive in `src/main.rs`).

Some features cost real compile time or native dependencies:

| Feature | Cost |
|---|---|
| `lancedb` | Needs the `protoc` compiler on `PATH` at build time |
| `local-embeddings` | Pulls the ONNX Runtime; unsupported on Windows MSVC; slow link step |
| `ui` | Builds the embedded Leptos UI as part of the crate; longest cold build of any single feature |
| `full-ui` | Everything above together; budget several minutes on a modest machine |

For a first install, stay on defaults plus what you actually call. Defaults already give you `postgres`, `openai`, `ares-vector`, `mcp`, `inventory`, and `rhai-policy`.

### Build offline or air-gapped

The repository ships no vendor directory. For an air-gapped machine, vendor dependencies on a connected machine first:

```bash
cd ares
cargo vendor vendor
```

Copy the whole tree, including `vendor/`, to the target machine. Then point Cargo at it through `.cargo/config.toml` next to `Cargo.toml`:

```toml
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
```

Build with `cargo build --release --offline`. Two notes apply:

- SQL migrations live inside the `ares-store` crate and ship inside the published package, so an offline build needs no external migration files.
- The default TLS stack uses rustls, so you need no system OpenSSL headers. If a non-default feature drags in OpenSSL on a host without `pkg-config`/`libssl-dev`, enable its vendored form in `Cargo.toml` (see the commented `vendored` example near the end of the dependency list) instead of installing system packages.

## Troubleshoot installation

| Symptom | Cause | Fix |
|---|---|---|
| `package \`ares-server v0.10.0\` cannot be built because it requires rustc 1.98 or newer` | Toolchain older than the declared `rust-version` | Run `rustup update stable`, then retry |
| Installed binary prints `requires the \`postgres\` feature` and exits 1 | Built or installed with `--no-default-features` or without `postgres` | Reinstall with `--features postgres`, or keep `default` |
| `error: failed to run custom build command` naming `protoc` | `lancedb` enabled without Protocol Buffers compiler | Install `protoc`, or drop `lancedb` from `--features` |
| Link errors mentioning ONNXRuntime under `local-embeddings` | Missing ONNX Runtime library, or Windows MSVC host | Install ONNX Runtime, or use a remote embeddings endpoint without the feature |
| `ares-server: command not found` after install | `$HOME/.cargo/bin` missing from `PATH` | Add `export PATH="$HOME/.cargo/bin:$PATH"` to your shell profile |
| Build succeeds but `/ui` returns 404 | `ui` feature absent from this binary | Rebuild with `--features ui` |

Compile-time versus run time matters here. Features such as `openai`, `azure`, or `bedrock` decide which provider code exists inside the binary. A provider that is absent at compile time cannot appear at run time by editing `ares.toml`. Configuration selects among compiled-in options; it never adds new ones.

## Verify the install

Print the version:

```console
$ ares-server --version
ares-server 0.10.0
```
