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
- ✅ **Web Search**: Built-in web search via daedra (no API keys required)
- ✅ **OpenAPI**: Automatic API documentation generation
- ✅ **Testing**: Comprehensive unit and integration tests

## Quick Start

### Prerequisites

- **Rust 1.75+**: Install via [rustup](https://rustup.rs/)
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

### Environment Variables

Create a `.env` file or set environment variables:

```bash
# Server
HOST=127.0.0.1
PORT=3000

# Database (local SQLite by default)
TURSO_URL=file:local.db
TURSO_AUTH_TOKEN=

# LLM Provider - Ollama (default)
OLLAMA_BASE_URL=http://localhost:11434
OLLAMA_MODEL=llama3.2

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
