# A.R.E.S - Agentic Retrieval Enhanced Server

![Ares Logo](./docs/ares.png)

A production-grade agentic chatbot server built in Rust with multi-provider LLM support, tool calling, RAG, MCP integration, and advanced research capabilities.

## Features

- ✅ **Multi-Provider LLM Support**: Ollama, OpenAI, LlamaCpp (direct GGUF loading)
- ✅ **TOML Configuration**: Declarative configuration with hot-reloading
- ✅ **Configurable Agents**: Define agents via TOML with custom models, tools, and prompts
- ✅ **Workflow Engine**: Declarative workflow execution with agent routing
- ✅ **Local-First Development**: Runs entirely locally with Ollama and SQLite by default
- ✅ **Tool Calling**: Type-safe function calling with automatic schema generation
- ✅ **Per-Agent Tool Filtering**: Restrict which tools each agent can access
- ✅ **Streaming**: Real-time streaming responses from all providers
- ✅ **Authentication**: JWT-based auth with Argon2 password hashing
- ✅ **Database**: Local SQLite (libsql) by default, optional Turso and Qdrant
- ✅ **MCP Support**: Pluggable Model Context Protocol server integration
- ✅ **Agent Framework**: Multi-agent orchestration with specialized agents
- ✅ **RAG**: Pluggable knowledge bases with semantic search
- ✅ **Memory**: User personalization and context management
- ✅ **Deep Research**: Multi-step research with parallel subagents
- ✅ **Web Search**: Built-in web search via [daedra](https://github.com/dirmacs/daedra) (no API keys required)
- ✅ **OpenAPI**: Automatic API documentation generation
- ✅ **Testing**: Comprehensive unit and integration tests
- ✅ **Config Validation**: Circular reference detection and unused config warnings

## Quick Start

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
ollama pull granite4:tiny-h
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

### Feature Bundles

| Feature | Includes |
|---------|----------|
| `all-llm` | ollama + openai + llamacpp |
| `all-db` | local-db + turso + qdrant |
| `full` | All optional features |
| `minimal` | No optional features |

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
default_model = "granite4:tiny-h"

[providers.openai]  # Optional
type = "openai"
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4"

# Models (reference providers, set parameters)
[models.fast]
provider = "ollama-local"
model = "granite4:tiny-h"
temperature = 0.7
max_tokens = 256

[models.balanced]
provider = "ollama-local"
model = "granite4:tiny-h"
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

### Legacy Environment Variables

For backward compatibility, these environment variables can also be used:

```bash
# Server
HOST=127.0.0.1
PORT=3000

# Database (local-first)
# Examples: ./data/ares.db | file:./data/ares.db | :memory:
DATABASE_URL=./data/ares.db

# Optional: Turso cloud (set both to enable)
# TURSO_URL=libsql://<your-db>-<your-org>.turso.io
# TURSO_AUTH_TOKEN=...

# LLM Provider - Ollama (default)
OLLAMA_URL=http://localhost:11434

# LLM Provider - OpenAI (optional)
# OPENAI_API_KEY=sk-...
# OPENAI_API_BASE=https://api.openai.com/v1
# OPENAI_MODEL=gpt-4

# LLM Provider - LlamaCpp (optional, highest priority if set)
# LLAMACPP_MODEL_PATH=/path/to/model.gguf

# Authentication
JWT_SECRET=your-secret-key-at-least-32-characters
API_KEY=your-api-key

# Optional: Qdrant for vector search
# QDRANT_URL=http://localhost:6334
# QDRANT_API_KEY=
```

### Provider Priority

When multiple providers are configured, they are selected in this order:

1. **LlamaCpp** - If `LLAMACPP_MODEL_PATH` is set
2. **OpenAI** - If `OPENAI_API_KEY` is set
3. **Ollama** - Default fallback (no API key required)

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
│  ┌────────┐                 │                                               │
│  │LlamaCpp│                 │                                               │
│  └────────┘                 │                                               │
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

## Tool Calling

A.R.E.S supports tool calling with Ollama models that support function calling (granite4:tiny-h+, mistral, etc.):

### Built-in Tools

- **calculator**: Basic arithmetic operations
- **web_search**: Web search via DuckDuckGo (no API key required)

### Tool Calling Example

```rust
use ares::llm::{OllamaClient, OllamaToolCoordinator};
use ares::tools::registry::ToolRegistry;
use ares::tools::{Calculator, WebSearch};

// Set up tools
let mut registry = ToolRegistry::new();
registry.register

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
- A model installed (e.g., `ollama pull granite4:tiny-h`)

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
just run            # Run server
just run-debug      # Run with debug logging

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

# Ollama
just ollama-pull    # Pull default model
just ollama-status  # Check if running

# Info
just info           # Show project info
just status         # Show environment status
```
