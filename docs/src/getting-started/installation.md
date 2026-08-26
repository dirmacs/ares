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

Enable features with `--features`:

```bash
cargo install ares-server --features full-ui
```

For an embed-only library build without server defaults, disable all features:

```bash
cargo build --no-default-features
```

## Verify the install

Print the version:

```console
$ ares-server --version
ares-server 0.10.0
```
