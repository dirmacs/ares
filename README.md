# ares

A production-grade agentic chatbot server built in Rust with multi-provider LLM support, tool calling, RAG, MCP integration, and advanced research capabilities.

## Features

- ✅ **Generic LLM Client**: Support for OpenAI, Anthropic, Ollama, llama.cpp
- ✅ **Authentication**: JWT-based auth with Argon2 password hashing
- ✅ **Database**: Turso for state/persistence, Qdrant for vector search
- ✅ **Tool Calling**: Type-safe function calling with automatic schema generation
- ✅ **MCP Support**: Pluggable Model Context Protocol server integration
- ✅ **Agent Framework**: Multi-agent orchestration with specialized agents
- ✅ **RAG**: Pluggable knowledge bases with semantic search
- ✅ **Memory**: User personalization and context management
- ✅ **Deep Research**: Multi-step research with parallel subagents
- ✅ **OpenAPI**: Automatic API documentation generation
- ✅ **Testing**: Comprehensive unit and integration tests

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
│  - OpenAI     │  │  - Search      │  │    Bases    │
│  - Ollama     │  │  - Calculator  │  │  - Qdrant   │
│  - llama.cpp  │  │  - Database    │  │  - Turso    │
└───────────────┘  └───────────────┘  └──────────────┘
```

## Quick Start

### Prerequisites

- Rust 1.75+
- Docker (for Qdrant)
- Turso account or local libSQL

### 1. Clone and Setup

```bash
git clone <repo>
cd agentic-chatbot-server
cp .env.example .env
```

### 2. Configure Environment

Edit `.env`:

```bash
# Required
TURSO_URL=libsql://your-database.turso.io
TURSO_AUTH_TOKEN=your_token
JWT_SECRET=your_secret_key
API_KEY=your_api_key

# At least one LLM provider
OPENAI_API_KEY=sk-...
# OR
OLLAMA_URL=http://localhost:11434
```

### 3. Start Qdrant

```bash
docker run -p 6334:6334 qdrant/qdrant
```

### 4. Build and Run

```bash
cargo build --release
cargo run
```

Server runs on `http://localhost:3000`

## API Documentation

Interactive Swagger UI available at: `http://localhost:3000/swagger-ui/`

### Authentication

#### Register
```bash
POST /api/auth/register
{
  "email": "user@example.com",
  "password": "secure_password",
  "name": "John Doe"
}
```

#### Login
```bash
POST /api/auth/login
{
  "email": "user@example.com",
  "password": "secure_password"
}

Response:
{
  "access_token": "eyJ...",
  "refresh_token": "eyJ...",
  "expires_in": 900
}
```

### Chat

```bash
POST /api/chat
Authorization: Bearer <access_token>
{
  "message": "What products do we have?",
  "agent_type": "product"
}

Response:
{
  "response": "Here are our current products...",
  "agent": "ProductAgent",
  "context_id": "uuid",
  "sources": [...]
}
```

### Deep Research

```bash
POST /api/research
Authorization: Bearer <access_token>
{
  "query": "Analyze market trends in renewable energy",
  "depth": 3,
  "max_iterations": 5
}

Response:
{
  "findings": "Comprehensive research report...",
  "sources": [...],
  "duration_ms": 45000
}
```

## Agent Types

- **Router**: Initial query classifier
- **Orchestrator**: Coordinates multiple agents
- **Product**: Product information and recommendations
- **Invoice**: Invoice processing and queries
- **Sales**: Sales data and analytics
- **Finance**: Financial analysis and reporting
- **HR**: Human resources queries

## Testing

### Run All Tests

```bash
cargo test
```

### Run with Coverage

```bash
cargo install cargo-llvm-cov
cargo llvm-cov --html --open
```

### Run Integration Tests Only

```bash
cargo test --test '*'
```

## Project Structure

```
src/
├── main.rs              # Application entry point
├── api/                 # API routes and handlers
├── agents/              # Agent implementations
├── llm/                 # LLM client abstractions
├── tools/               # Tool calling framework
├── mcp/                 # MCP integration
├── rag/                 # RAG components
├── db/                  # Database clients
├── auth/                # Authentication
├── memory/              # User memory system
├── research/            # Deep research
├── types/               # Type definitions
└── utils/               # Utilities
```

## Configuration

### LLM Providers

The server supports multiple LLM providers simultaneously:

```rust
// In your code, select provider dynamically
let provider = Provider::OpenAI {
    api_key: config.llm.openai_api_key.unwrap(),
    model: "gpt-4".to_string(),
};

let client = provider.create_client().await?;
```

### Custom Tools

Add custom tools by implementing the `Tool` trait:

```rust
use crate::tools::Tool;

struct MyCustomTool;

#[async_trait]
impl Tool for MyCustomTool {
    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "My custom tool" }

    async fn execute(&self, args: serde_json::Value) -> Result<serde_json::Value> {
        // Implementation
    }
}
```

### Knowledge Bases

Implement custom knowledge bases:

```rust
use crate::rag::KnowledgeBase;

struct MyKnowledgeBase;

#[async_trait]
impl KnowledgeBase for MyKnowledgeBase {
    async fn search(&self, query: &str) -> Result<Vec<Document>> {
        // Implementation
    }
}
```

## Performance

- **Latency**: <100ms for simple queries
- **Throughput**: 1000+ req/sec on modern hardware
- **Memory**: ~50MB base + ~1MB per concurrent request
- **Database**: Supports 100K+ documents with sub-50ms search

## Security

- Argon2 password hashing (OWASP recommended)
- JWT with RS256 (asymmetric) for production
- Token rotation for refresh tokens
- Rate limiting on auth endpoints
- Input validation on all endpoints
- Secure random number generation

## Monitoring

Structured logging with tracing:

```bash
RUST_LOG=info cargo run
```

OpenTelemetry integration for production monitoring.

## Deployment

### Docker

```dockerfile
FROM rust:1.75 as builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/agentic-chatbot-server /usr/local/bin/
CMD ["agentic-chatbot-server"]
```

### Environment Variables

All configuration via environment variables - see `.env.example`

## Development

### Adding a New Agent

1. Create agent file in `src/agents/`
2. Implement `Agent` trait
3. Register in agent graph
4. Add to `AgentType` enum
5. Update router logic

### Adding a New Tool

1. Create tool file in `src/tools/`
2. Implement `Tool` trait
3. Register in tool registry
4. Add schema with `schemars`

## Troubleshooting

### Qdrant Connection Error

Ensure Qdrant is running:
```bash
docker ps | grep qdrant
```

### Database Migration Issues

Reset Turso database:
```bash
turso db shell <database-name>
DROP TABLE IF EXISTS users;
# Restart server to recreate
```

### JWT Errors

Regenerate secret:
```bash
openssl rand -base64 32
```

## Contributing

1. Fork the repository
2. Create feature branch
3. Add tests for new functionality
4. Ensure `cargo test` passes
5. Run `cargo fmt` and `cargo clippy`
6. Submit pull request

## License

MIT

## Acknowledgments

- Built with [Rig.rs](https://github.com/0xPlaygrounds/rig)
- Inspired by LangChain and AutoGPT
- Uses production patterns from Anthropic's research
