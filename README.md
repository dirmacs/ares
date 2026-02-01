# A.R.E.S - Agentic Retrieval Enhanced Server

[![Crates.io](https://img.shields.io/crates/v/ares-server.svg)](https://crates.io/crates/ares-server)
[![Documentation](https://docs.rs/ares-server/badge.svg)](https://docs.rs/ares-server)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.91%2B-blue.svg)](https://www.rust-lang.org)
[![CI](https://github.com/dirmacs/ares/actions/workflows/ci.yml/badge.svg)](https://github.com/dirmacs/ares/actions/workflows/ci.yml)

![Ares Logo](./docs/ares.png)

A production-grade agentic chatbot server built in Rust with multi-provider LLM support, tool calling, RAG, MCP integration, and advanced research capabilities.

## Features

- 🤖 **Multi-Provider LLM Support**: Ollama, OpenAI, Anthropic Claude, LlamaCpp (direct GGUF loading)
- ⚙️ **TOML Configuration**: Declarative configuration with hot-reloading
- 🎭 **Configurable Agents**: Define agents via [TOON (Token Oriented Object Notation)](https://toonformat.dev) with custom models, tools, and prompts
- 🔄 **Workflow Engine**: Declarative workflow execution with agent routing
- 🏠 **Local-First Development**: Runs entirely locally with Ollama and SQLite by default
- 🔧 **Tool Calling**: Type-safe function calling with automatic schema generation
- 🔄 **Unified ToolCoordinator**: Provider-agnostic multi-turn tool calling for all LLM clients
- 🎯 **Per-Agent Tool Filtering**: Restrict which tools each agent can access
- 📡 **Streaming**: Real-time streaming responses from all providers
- 🔐 **Authentication**: JWT-based auth with Argon2 password hashing
- 💾 **Database**: Local SQLite (libsql) by default, optional Turso and Qdrant
- 🔌 **MCP Support**: Pluggable Model Context Protocol server integration
- 🕸️ **Agent Framework**: Multi-agent orchestration with specialized agents
- 📚 **RAG**: Pure-Rust vector store (ares-vector), multi-strategy search (semantic, BM25, fuzzy, hybrid), reranking
- 🧠 **Memory**: User personalization and context management
- 🔬 **Deep Research**: Multi-step research with parallel subagents
- 🌐 **Web Search**: Built-in web search via [daedra](https://github.com/dirmacs/daedra) (no API keys required)
- 📖 **OpenAPI**: Automatic API documentation generation
- 🧪 **Testing**: Comprehensive unit and integration tests
- ✔️ **Config Validation**: Circular reference detection and unused config warnings

## Installation

A.R.E.S can be used as a **standalone server** or as a **library** in your Rust project.

### As a Library

Add to your project:

```bash
cargo add ares-server
```

Basic usage:

```rust
use ares::{Provider, LLMClient};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create an Ollama provider
    let provider = Provider::Ollama {
        base_url: "http://localhost:11434".to_string(),
        model: "llama3.2:3b".to_string(),
    };

    // Create a client and generate a response
    let client = provider.create_client().await?;
    let response = client.generate("Hello, world!").await?;
    println!("{}", response);

    Ok(())
}
```

### As a Binary

```bash
# Install from crates.io (basic installation)
cargo install ares-server

# Install with embedded Web UI
cargo install ares-server --features ui

# Initialize a new project (creates ares.toml and config files)
ares-server init

# Run the server
ares-server
```

## CLI Commands

A.R.E.S provides a full-featured CLI with colored output:

```bash
# Initialize a new project with all configuration files
ares-server init

# Initialize with custom options
ares-server init --provider openai --port 8080 --host 0.0.0.0

# Initialize with minimal configuration
ares-server init --minimal

# View configuration summary
ares-server config

# Validate configuration
ares-server config --validate

# List all configured agents
ares-server agent list

# Show details for a specific agent
ares-server agent show orchestrator

# Start the server
ares-server

# Start with verbose logging
ares-server --verbose

# Use a custom config file
ares-server --config custom.toml

# Disable colored output
ares-server --no-color init
```

### Init Command Options

| Option | Description |
|--------|-------------|
| `--force, -f` | Overwrite existing files |
| `--minimal, -m` | Create minimal configuration |
| `--no-examples` | Skip creating TOON example files |
| `--provider <NAME>` | LLM provider: `ollama`, `openai`, or `both` |
| `--host <ADDR>` | Server host address (default: 127.0.0.1) |
| `--port <PORT>` | Server port (default: 3000) |

## Quick Start (Development)

### Prerequisites

- **Rust 1.91+**: Install via [rustup](https://rustup.rs/)
- **Ollama** (recommended): For local LLM inference - [Install Ollama](https://ollama.ai)
- **just** (recommended): Command runner - [Install just](https://just.systems)

### 1. Clone and Setup

```bash
git clone https://github.com/dirmacs/ares.git
cd ares
cp .env.example .env

# Or use just to set up everything:
just setup
```

### 2. Start Ollama (Recommended)

```bash
# Install a model
ollama pull ministral-3:3b
# Or: just ollama-pull

# Ollama runs automatically as a service, or start manually:
ollama serve
```

### 3. Build and Run

```bash
# Build with default features (local-db + ollama)
cargo build
# Or: just build

# Run the server
cargo run
# Or: just run
```

Server runs on `http://localhost:3000`

## Feature Flags

A.R.E.S uses Cargo features for conditional compilation:

### LLM Providers

| Feature | Description | Default |
|---------|-------------|---------|
| `ollama` | Ollama local inference | ✅ Yes |
| `openai` | OpenAI API (and compatible) | No |
| `anthropic` | Anthropic Claude API | No |
| `llamacpp` | Direct GGUF model loading | No |
| `llamacpp-cuda` | LlamaCpp with CUDA | No |
| `llamacpp-metal` | LlamaCpp with Metal (macOS) | No |
| `llamacpp-vulkan` | LlamaCpp with Vulkan | No |

### Database Backends

| Feature | Description | Default |
|---------|-------------|---------|
| `local-db` | Local SQLite via libsql | ✅ Yes |
| `turso` | Remote Turso database | No |
| `qdrant` | Qdrant vector database | No |
| `ares-vector` | Pure-Rust vector store with HNSW indexing | No |

### UI & Documentation

| Feature | Description | Default |
|---------|-------------|---------|
| `ui` | Embedded Leptos web UI served from backend | No |
| `swagger-ui` | Interactive API documentation at `/swagger-ui/` | No |

> **Note:** `swagger-ui` was made optional in v0.2.5 to reduce binary size and build time. The feature requires network access during build to download Swagger UI assets.

### Embeddings

| Feature | Description | Default |
|---------|-------------|---------|
| `local-embeddings` | Local ONNX embedding models via fastembed | No |

> **Warning:** The `local-embeddings` feature does **NOT** work on Windows MSVC due to `ort-sys` linker errors. Use WSL, Linux, or macOS for local embeddings, or use remote embedding APIs instead.

### Feature Bundles

| Feature | Includes |
|---------|----------|
| `all-llm` | ollama + openai + llamacpp + anthropic |
| `all-db` | local-db + turso + qdrant |
| `full` | All optional features (except UI and local-embeddings): ollama, openai, llamacpp, anthropic, turso, qdrant, ares-vector, mcp, swagger-ui |
| `full-ui` | All optional features + UI (except local-embeddings) |
| `full-local-embeddings` | Full + local-embeddings (Linux/macOS only) |
| `full-ui-local-embeddings` | Full + UI + local-embeddings (Linux/macOS only) |
| `minimal` | No optional features |

> **Note:** `local-embeddings` is excluded from `full` and `full-ui` bundles due to Windows MSVC compatibility issues. Use `full-local-embeddings` or `full-ui-local-embeddings` on Linux/macOS.

### Building with Features

```bash
# Default (ollama + local-db)
cargo build
# Or: just build

# With OpenAI support
cargo build --features "openai"
# Or: just build-features "openai"

# With direct GGUF loading
cargo build --features "llamacpp"

# With CUDA GPU acceleration
cargo build --features "llamacpp-cuda"

# Full feature set
cargo build --features "full"
# Or: just build-all

# With embedded Web UI
cargo build --features "ui"

# With Swagger UI (interactive API docs)
cargo build --features "swagger-ui"

# Full feature set with UI
cargo build --features "full-ui"

# Release build
cargo build --release
# Or: just build-release
```

## Configuration

A.R.E.S uses a **TOML configuration file** (`ares.toml`) for declarative configuration of all components. The server **requires** this file to start.

### Quick Start

```bash
# Copy the example config
cp ares.example.toml ares.toml

# Set required environment variables
export JWT_SECRET="your-secret-key-at-least-32-characters"
export API_KEY="your-api-key"
```

### Configuration File (ares.toml)

The configuration file defines providers, models, agents, tools, and workflows:

```toml
# Server settings
[server]
host = "127.0.0.1"
port = 3000
log_level = "info"

# Authentication (secrets loaded from env vars)
[auth]
jwt_secret_env = "JWT_SECRET"
api_key_env = "API_KEY"

# Database
[database]
url = "./data/ares.db"

# LLM Providers (define named providers)
[providers.ollama-local]
type = "ollama"
base_url = "http://localhost:11434"
default_model = "ministral-3:3b"

[providers.openai]  # Optional
type = "openai"
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4"

# Models (reference providers, set parameters)
[models.fast]
provider = "ollama-local"
model = "ministral-3:3b"
temperature = 0.7
max_tokens = 256

[models.balanced]
provider = "ollama-local"
model = "ministral-3:3b"
temperature = 0.7
max_tokens = 512

[models.smart]
provider = "ollama-local"
model = "qwen3-vl:2b"
temperature = 0.3
max_tokens = 1024

# Tools (define available tools)
[tools.calculator]
enabled = true
timeout_secs = 10

[tools.web_search]
enabled = true
timeout_secs = 30

# Agents (reference models and tools)
[agents.router]
model = "fast"
system_prompt = "You route requests to specialized agents..."

[agents.product]
model = "balanced"
tools = ["calculator"]                     # Tool filtering: only calculator
system_prompt = "You are a Product Agent..."

[agents.research]
model = "smart"
tools = ["web_search", "calculator"]       # Multiple tools
system_prompt = "You conduct research..."

# Workflows (define agent routing)
[workflows.default]
entry_agent = "router"
fallback_agent = "product"
max_depth = 5

[workflows.research_flow]
entry_agent = "research"
max_depth = 10
```

### Per-Agent Tool Filtering

Each agent can specify which tools it has access to:

```toml
[agents.restricted]
model = "balanced"
tools = ["calculator"]  # Only calculator, no web search

[agents.full_access]
model = "balanced"
tools = ["calculator", "web_search"]  # Both tools
```

If `tools` is empty or omitted, the agent has no tool access.

### Configuration Validation

The configuration is validated on load with:

- **Reference checking**: Models must reference valid providers, agents must reference valid models
- **Circular reference detection**: Workflows cannot have circular agent references
- **Environment variables**: All referenced env vars must be set

For warnings about unused configuration items (providers, models, tools not referenced by anything), the `validate_with_warnings()` method is available.

### Hot Reloading

Configuration changes are **automatically detected** and applied without restarting the server. Edit `ares.toml` and the changes will be picked up within 500ms.

### Environment Variables

The following environment variables **must** be set (referenced by `ares.toml`):

```bash
# Required
JWT_SECRET=your-secret-key-at-least-32-characters
API_KEY=your-api-key

# Optional (for OpenAI provider)
OPENAI_API_KEY=sk-...
```

### Provider Priority

When multiple providers are configured, they are selected in this order:

1. **LlamaCpp** - If `LLAMACPP_MODEL_PATH` is set
2. **OpenAI** - If `OPENAI_API_KEY` is set
3. **Ollama** - Default fallback (no API key required)

### Dynamic Configuration (TOON)

In addition to `ares.toml`, A.R.E.S supports **TOON (Token Oriented Object Notation)** files for behavioral configuration with hot-reloading:

```
config/
├── agents/
│   ├── router.toon
│   ├── orchestrator.toon
│   └── product.toon
├── models/
│   ├── fast.toon
│   └── balanced.toon
├── tools/
│   └── calculator.toon
├── workflows/
│   └── default.toon
└── mcps/
    └── filesystem.toon
```

**Example TOON agent config** (`config/agents/router.toon`):

```toon
name: router
model: fast
max_tool_iterations: 5
parallel_tools: false
tools[0]:
system_prompt: |
  You are a router agent that directs requests to specialized agents.
```

**Enable TOON configs** in `ares.toml`:

```toml
[config]
agents_dir = "config/agents"
models_dir = "config/models"
tools_dir = "config/tools"
workflows_dir = "config/workflows"
mcps_dir = "config/mcps"
hot_reload = true
```

TOON files are automatically hot-reloaded when changed. See [docs/DIR-12-research.md](docs/DIR-12-research.md) for details.

### User-Created Agents API

Users can create custom agents stored in the database with TOON import/export:

```bash
# Create a custom agent
curl -X POST http://localhost:3000/api/agents \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "my-agent",
    "model": "balanced",
    "system_prompt": "You are a helpful assistant.",
    "tools": ["calculator"]
  }'

# Export as TOON
curl http://localhost:3000/api/agents/{id}/export \
  -H "Authorization: Bearer $TOKEN"

# Import from TOON
curl -X POST http://localhost:3000/api/agents/import \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: text/plain" \
  -d 'name: imported-agent
model: fast
system_prompt: |
  You are an imported agent.'
```

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                            ares.toml (Configuration)                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐     │
│  │providers │  │ models   │  │ agents   │  │  tools   │  │workflows │     │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘  └──────────┘     │
└──────────────────────────────┬──────────────────────────────────────────────┘
                               │ Hot Reload
┌──────────────────────────────▼──────────────────────────────────────────────┐
│                         AresConfigManager                                    │
│                    (Thread-safe config access)                               │
└──────────────────────────────┬──────────────────────────────────────────────┘
                               │
       ┌───────────────────────┼───────────────────────────┐
       │                       │                           │
┌──────▼──────┐         ┌──────▼──────┐            ┌──────▼──────┐
│  Provider   │         │    Agent    │            │    Tool     │
│  Registry   │         │  Registry   │            │  Registry   │
└──────┬──────┘         └──────┬──────┘            └──────┬──────┘
       │                       │                          │
       │                ┌──────▼──────┐                   │
       │                │Configurable │◄──────────────────┘
       │                │   Agent     │  (filtered tools)
       │                └──────┬──────┘
       │                       │
┌──────▼───────────────────────┼──────────────────────────────────────────────┐
│      LLM Clients             │                                               │
│  ┌────────┐ ┌────────┐      │                                               │
│  │Ollama  │ │OpenAI  │      │                                               │
│  └────────┘ └────────┘      │                                               │
│  ┌────────┐ ┌────────┐      │                                               │
│  │LlamaCpp│ │Anthropic│     │                                               │
│  └────────┘ └────────┘      │                                               │
└─────────────────────────────┼───────────────────────────────────────────────┘
                              │
┌─────────────────────────────▼───────────────────────────────────────────────┐
│                         Workflow Engine                                      │
│  ┌─────────────┐    execute_workflow()    ┌─────────────┐                  │
│  │  Workflow   │─────────────────────────▶│  Agent      │                  │
│  │  Config     │                          │  Execution  │                  │
│  └─────────────┘                          └─────────────┘                  │
└─────────────────────────────────────────────────────────────────────────────┘
                              │
      ┌───────────────────────┼───────────────────┐
      │                       │                   │
┌─────▼─────────┐     ┌──────▼───────┐    ┌─────▼───────┐
│  API Layer    │     │ Tool Calls   │    │  Knowledge  │
│  (Axum)       │     │              │    │    Bases    │
│ /api/chat     │     │ - Calculator │    │  - SQLite   │
│ /api/research │     │ - Web Search │    │  - Qdrant   │
│ /api/workflows│     │              │    │             │
└───────────────┘     └──────────────┘    └─────────────┘
```

### Key Components

- **AresConfigManager**: Thread-safe configuration management with hot-reloading
- **ProviderRegistry**: Creates LLM clients based on model configuration  
- **AgentRegistry**: Creates ConfigurableAgents from TOML configuration
- **ToolRegistry**: Manages available tools and their configurations
- **ConfigurableAgent**: Generic agent implementation that uses config for behavior
- **WorkflowEngine**: Executes declarative workflows defined in TOML

## API Documentation

Interactive Swagger UI available at: `http://localhost:3000/swagger-ui/`

> **Note:** Swagger UI requires the `swagger-ui` feature to be enabled at build time:
> ```bash
> cargo build --features "swagger-ui"
> # Or use the full bundle:
> cargo build --features "full"
> ```

### Authentication

#### Register
```bash
curl -X POST http://localhost:3000/api/auth/register \
  -H "Content-Type: application/json" \
  -d '{
    "email": "user@example.com",
    "password": "secure_password",
    "name": "John Doe"
  }'
```

#### Login
```bash
curl -X POST http://localhost:3000/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{
    "email": "user@example.com",
    "password": "secure_password"
  }'
```

Response:
```json
{
  "access_token": "eyJ...",
  "refresh_token": "eyJ...",
  "expires_in": 900
}
```

### Chat

```bash
curl -X POST http://localhost:3000/api/chat \
  -H "Authorization: Bearer <access_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "message": "What products do we have?",
    "agent_type": "product"
  }'
```

### Deep Research

```bash
curl -X POST http://localhost:3000/api/research \
  -H "Authorization: Bearer <access_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "Analyze market trends in renewable energy",
    "depth": 3,
    "max_iterations": 5
  }'
```

### Workflows

Workflows enable multi-agent orchestration. Define workflows in `ares.toml`:

```toml
[workflows.default]
entry_agent = "router"           # Starting agent
fallback_agent = "orchestrator"  # Used if routing fails
max_depth = 5                    # Maximum agent chain depth
max_iterations = 10              # Maximum total iterations
```

#### List Available Workflows

```bash
curl http://localhost:3000/api/workflows \
  -H "Authorization: Bearer <access_token>"
```

Response:
```json
["default", "research"]
```

#### Execute a Workflow

```bash
curl -X POST http://localhost:3000/api/workflows/default \
  -H "Authorization: Bearer <access_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "What are our Q4 product sales figures?"
  }'
```

Response:
```json
{
  "final_response": "Based on the Q4 data, our product sales were...",
  "steps_executed": 3,
  "agents_used": ["router", "sales", "product"],
  "reasoning_path": [
    {
      "agent_name": "router",
      "input": "What are our Q4 product sales figures?",
      "output": "sales",
      "timestamp": 1702500000,
      "duration_ms": 150
    },
    {
      "agent_name": "sales",
      "input": "What are our Q4 product sales figures?",
      "output": "For Q4 sales data, I'll need to check...",
      "timestamp": 1702500001,
      "duration_ms": 800
    },
    {
      "agent_name": "product",
      "input": "What are our Q4 product sales figures?",
      "output": "Based on the Q4 data, our product sales were...",
      "timestamp": 1702500002,
      "duration_ms": 650
    }
  ]
}
```

#### Workflow with Context

```bash
curl -X POST http://localhost:3000/api/workflows/default \
  -H "Authorization: Bearer <access_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "What are the sales figures?",
    "context": {
      "department": "electronics",
      "quarter": "Q4"
    }
  }'
```

### RAG (Retrieval Augmented Generation)

A.R.E.S includes a comprehensive RAG system with a pure-Rust vector store. Requires the `ares-vector` feature.

#### Ingest Documents

```bash
curl -X POST http://localhost:3000/api/rag/ingest \
  -H "Authorization: Bearer <access_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "collection": "docs",
    "content": "Your document content here...",
    "metadata": {"source": "manual", "category": "technical"},
    "chunking_strategy": "word"
  }'
```

#### Search Documents

```bash
curl -X POST http://localhost:3000/api/rag/search \
  -H "Authorization: Bearer <access_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "collection": "docs",
    "query": "What is the architecture?",
    "strategy": "hybrid",
    "top_k": 5,
    "rerank": true
  }'
```

**Search Strategies**:
- `semantic`: Vector similarity search
- `bm25`: Traditional keyword matching
- `fuzzy`: Typo-tolerant search
- `hybrid`: Weighted combination of semantic + BM25

#### List Collections

```bash
curl http://localhost:3000/api/rag/collections \
  -H "Authorization: Bearer <access_token>"
```

## Tool Calling

A.R.E.S supports tool calling with all LLM providers that support function calling (OpenAI, Anthropic, Ollama with ministral-3:3b+, etc.):

### Built-in Tools

- **calculator**: Basic arithmetic operations
- **web_search**: Web search via DuckDuckGo (no API key required)

### Unified ToolCoordinator

The `ToolCoordinator` provides a provider-agnostic way to handle multi-turn tool calling with any `LLMClient`:

```rust
use ares::llm::{Provider, ToolCoordinator, ToolCallingConfig};
use ares::tools::ToolRegistry;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create an LLM client (works with any provider)
    let provider = Provider::from_env()?;
    let client = provider.create_client().await?;

    // Set up tool registry with built-in tools
    let registry = Arc::new(ToolRegistry::new());

    // Create the unified coordinator
    let coordinator = ToolCoordinator::new(
        client,
        registry,
        ToolCallingConfig::default(),
    );

    // Execute a tool-calling conversation
    let result = coordinator.execute(
        Some("You are a helpful assistant with access to tools."),
        "What is 25 * 4?"
    ).await?;

    println!("Response: {}", result.content);
    println!("Tool calls made: {}", result.tool_calls.len());
    println!("Iterations: {}", result.iterations);

    Ok(())
}
```

### ToolCallingConfig Options

| Option | Default | Description |
|--------|---------|-------------|
| `max_iterations` | 10 | Maximum LLM round-trips before stopping |
| `parallel_execution` | true | Execute multiple tool calls in parallel |
| `tool_timeout` | 30s | Timeout for individual tool execution |
| `include_tool_results` | true | Include tool results in final context |
| `stop_on_error` | false | Stop on first tool error vs continue |

## Testing

A.R.E.S has comprehensive test coverage with both mocked and live tests.

### Unit & Integration Tests (Mocked)

```bash
# Run all tests (no external services required)
cargo test
# Or: just test

# Run with verbose output
cargo test -- --nocapture
# Or: just test-verbose
```

### Live Ollama Tests

Tests that connect to a **real Ollama instance** are available but **ignored by default**.

#### Prerequisites
- Running Ollama server at `http://localhost:11434`
- A model installed (e.g., `ollama pull ministral-3:3b`)

#### Running Live Tests

```bash
# Set the environment variable and run ignored tests
OLLAMA_LIVE_TESTS=1 cargo test --test ollama_live_tests -- --ignored
# Or: just test-ignored

# All tests (normal + ignored)
just test-all

# With verbose output
just test-all-verbose

# With custom Ollama URL or model
OLLAMA_URL=http://192.168.1.100:11434 OLLAMA_MODEL=mistral OLLAMA_LIVE_TESTS=1 \
  cargo test --test ollama_live_tests -- --ignored
```

Or add `OLLAMA_LIVE_TESTS=1` to your `.env` file.

### API Tests (Hurl)

End-to-end API tests using [Hurl](https://hurl.dev):

```bash
# Install Hurl
brew install hurl  # macOS

# Run API tests (server must be running)
just hurl

# Run with verbose output
just hurl-verbose

# Run specific test group
just hurl-health
just hurl-auth
just hurl-chat
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for more testing details.

## Common Commands (just)

A.R.E.S uses [just](https://just.systems) as a command runner. Run `just --list` to see all available commands:

```bash
# Show all commands
just --list

# Build & Run
just build          # Build (debug)
just build-release  # Build (release)
just build-ui       # Build with embedded UI
just run            # Run server
just run-ui         # Run with embedded UI
just run-debug      # Run with debug logging

# CLI Commands
just init           # Initialize project (ares-server init)
just init-openai    # Initialize with OpenAI provider
just config         # Show configuration summary
just agents         # List all agents
just agent <name>   # Show agent details

# Testing
just test           # Run tests
just test-verbose   # Run tests with output
just test-ignored   # Run live Ollama tests
just test-all       # Run all tests
just hurl           # Run API tests

# Code Quality
just lint           # Run clippy
just fmt            # Format code
just quality        # Run all quality checks

# Docker
just docker-up      # Start dev services
just docker-down    # Stop services
just docker-logs    # View logs

# UI Development
just ui-setup       # Install UI dependencies
just ui-dev         # Run UI dev server
just ui-build       # Build UI for production
just dev            # Run backend + UI together

# Ollama
just ollama-pull    # Pull default model
just ollama-status  # Check if running

# Info
just info           # Show project info
just status         # Show environment status
```

## Troubleshooting

### Configuration File Not Found

```bash
# Error: Configuration file 'ares.toml' not found!

# Solution: Initialize a new project
ares-server init
```

### Port Already in Use

```bash
# Error: Address already in use (os error 48)

# Find the process using port 3000
lsof -i :3000          # Linux/macOS
netstat -ano | findstr :3000  # Windows

# Kill the process
kill -9 <PID>          # Linux/macOS
taskkill /PID <PID> /F # Windows
```

### Ollama Connection Failed

```bash
# Check if Ollama is running
curl http://localhost:11434/api/tags

# Start Ollama
ollama serve

# Or start via Docker
just docker-services
```

### Missing Environment Variables

```bash
# Error: MissingEnvVar("JWT_SECRET")

# Solution: Set up environment variables
cp .env.example .env
# Edit .env and set JWT_SECRET (min 32 characters) and API_KEY
```

### UI Build Errors (Node.js runtime required)

```bash
# Error: npx: command not found

# Solution: Install a Node.js runtime
# Option 1: Install Bun (recommended)
curl -fsSL https://bun.sh/install | bash

# Option 2: Install Node.js
brew install node  # macOS
# or download from https://nodejs.org
```

### WASM Build Errors

```bash
# Error: target `wasm32-unknown-unknown` not found

# Solution: Add the WASM target
rustup target add wasm32-unknown-unknown

# Install trunk
cargo install trunk --locked
```

## Requirements

### Minimum Requirements

- **Rust**: 1.91 or later
- **Operating System**: Linux, macOS, or Windows
- **Memory**: 2GB RAM (4GB+ recommended for larger models)

### Optional Requirements

- **Ollama**: For local LLM inference (recommended)
- **Node.js runtime**: Bun, npm, or Deno (required for UI development)
- **Docker**: For containerized deployment
- **GPU**: NVIDIA (CUDA) or Apple Silicon (Metal) for accelerated inference

## Security Considerations

- **JWT_SECRET**: Must be at least 32 characters. Generate with: `openssl rand -base64 32`
- **API_KEY**: Should be unique per deployment
- **Environment Variables**: Never commit `.env` files to version control
- **HTTPS**: Use HTTPS in production (configure via reverse proxy)
- **Rate Limiting**: Consider adding rate limiting for production deployments

## Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

### Quick Contribution Guide

```bash
# 1. Fork and clone the repository
git clone https://github.com/YOUR_USERNAME/ares.git
cd ares

# 2. Create a feature branch
git checkout -b feature/my-feature

# 3. Make your changes and run tests
cargo fmt
cargo clippy
cargo test

# 4. Commit and push
git commit -m "feat: add my feature"
git push origin feature/my-feature

# 5. Open a Pull Request
```

### Development Setup

```bash
# Install development dependencies
just setup

# Run pre-commit checks before pushing
just pre-commit
```

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for a list of changes in each version.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Acknowledgments

- [Ollama](https://ollama.ai/) - Local LLM inference
- [llama.cpp](https://github.com/ggerganov/llama.cpp) - GGUF model support
- [Axum](https://github.com/tokio-rs/axum) - Web framework
- [Leptos](https://leptos.dev/) - Reactive web UI framework
- [TOON Format](https://toonformat.dev) - Token-optimized configuration format

## Support

- 📖 [Documentation](https://docs.rs/ares-server)
- 🐛 [Issue Tracker](https://github.com/dirmacs/ares/issues)
- 💬 [Discussions](https://github.com/dirmacs/ares/discussions)
- 🚀 [Latest Release](https://github.com/dirmacs/ares/releases)

---

Made with ❤️ by [Dirmacs](https://dirmacs.com)
