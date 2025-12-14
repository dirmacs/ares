# A.R.E.S - Agentic Retrieval Enhanced Server

A production-grade agentic chatbot server built in Rust with multi-provider LLM support, tool calling, RAG, MCP integration, and advanced research capabilities.

## Features

- ✅ **Multi-Provider LLM Support**: Ollama, OpenAI, LlamaCpp (direct GGUF loading)
- ✅ **Local-First Development**: Runs entirely locally with Ollama and SQLite by default
- ✅ **Tool Calling**: Type-safe function calling with automatic schema generation
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

## Quick Start

### Prerequisites

- **Rust 1.91+**: Install via [rustup](https://rustup.rs/)
- **Ollama** (recommended): For local LLM inference - [Install Ollama](https://ollama.ai)

### 1. Clone and Setup

```bash
git clone <repo>
cd ares
cp .env.example .env
```

### 2. Start Ollama (Recommended)

```bash
# Install a model
ollama pull llama3.2

# Ollama runs automatically as a service, or start manually:
ollama serve
```

### 3. Build and Run

```bash
# Build with default features (local-db + ollama)
cargo build

# Run the server
cargo run
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

# With OpenAI support
cargo build --features "openai"

# With direct GGUF loading
cargo build --features "llamacpp"

# With CUDA GPU acceleration
cargo build --features "llamacpp-cuda"

# Full feature set
cargo build --features "full"
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
default_model = "llama3.2"

# Models (reference providers, set parameters)
[models.fast]
provider = "ollama-local"
model = "llama3.2:1b"
temperature = 0.7
max_tokens = 256

[models.balanced]
provider = "ollama-local"
model = "llama3.2"
temperature = 0.7
max_tokens = 512

# Tools
[tools.calculator]
enabled = true
timeout_secs = 10

[tools.web_search]
enabled = true
timeout_secs = 30

# Agents (reference models and tools)
[agents.router]
model = "fast"
system_prompt = "You are a routing agent..."

[agents.product]
model = "balanced"
tools = []
system_prompt = "You are a Product Agent..."

# Workflows
[workflows.default]
entry_agent = "router"
fallback_agent = "orchestrator"
```

See `ares.toml` for the complete configuration with all options documented.

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
┌─────────────┐
│   Client    │
└──────┬──────┘
       │
┌──────▼──────────────────────────────────────┐
│           API Layer (Axum)                   │
│  - Authentication Middleware                 │
│  - OpenAPI Documentation                     │
└──────┬──────────────────────────────────────┘
       │
┌──────▼──────────────────────────────────────┐
│         Agent Graph Workflow                 │
│                                              │
│  ┌─────────┐    ┌──────────────┐           │
│  │ Router  │───▶│ Orchestrator │           │
│  └─────────┘    └───────┬──────┘           │
│                          │                   │
│         ┌────────────────┼────────────┐     │
│         │                │            │     │
│    ┌────▼────┐     ┌────▼────┐  ┌───▼───┐ │
│    │ Product │     │ Invoice │  │   HR  │ │
│    │  Agent  │     │  Agent  │  │ Agent │ │
│    └─────────┘     └─────────┘  └───────┘ │
│         │                │            │     │
│         └────────────────┼────────────┘     │
│                          │                   │
└──────────────────────────┼───────────────────┘
                           │
       ┌───────────────────┼──────────────────┐
       │                   │                  │
┌──────▼────────┐  ┌───────▼───────┐  ┌──────▼──────┐
│  LLM Clients  │  │  Tool Registry │  │  Knowledge  │
│  - Ollama     │  │  - Web Search  │  │    Bases    │
│  - OpenAI     │  │  - Calculator  │  │  - SQLite   │
│  - LlamaCpp   │  │  - Database    │  │  - Qdrant   │
└───────────────┘  └───────────────┘  └──────────────┘
```

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

## Tool Calling

A.R.E.S supports tool calling with Ollama models that support function calling (llama3.1+, mistral, etc.):

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

# Run with verbose output
cargo test -- --nocapture
```

### Live Ollama Tests

Tests that connect to a **real Ollama instance** are available but **ignored by default**.

#### Prerequisites
- Running Ollama server at `http://localhost:11434`
- A model installed (e.g., `ollama pull llama3.2`)

#### Running Live Tests

```bash
# Set the environment variable and run ignored tests
OLLAMA_LIVE_TESTS=1 cargo test --test ollama_live_tests -- --ignored

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
cd scripts/hurl && nu run.nu
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for more testing details.