# A.R.E.S Quick Reference Card

Fast reference for common development tasks and commands.

## 🚀 Quick Start

```bash
# Option 1: Install from crates.io
cargo install ares-server
ares-server init
ares-server

# Option 2: Install with embedded Web UI
cargo install ares-server --features ui
ares-server init
ares-server  # UI available at http://localhost:3000/

# Option 3: Clone and develop
git clone <repo>
cd ares
just setup
just run
```

## 🖥️ CLI Commands

```bash
# Initialize a new project
ares-server init                          # Create ares.toml and config/
ares-server init --provider openai        # Use OpenAI instead of Ollama
ares-server init --provider both          # Configure both providers
ares-server init --host 0.0.0.0 --port 8080  # Custom host/port
ares-server init --force                  # Overwrite existing files
ares-server init --minimal                # Minimal configuration
ares-server init --no-examples            # Skip TOON example files

# View configuration
ares-server config                        # Show config summary
ares-server config --full                 # Show full configuration
ares-server config --validate             # Validate configuration

# Manage agents
ares-server agent list                    # List all agents
ares-server agent show orchestrator       # Show agent details

# Run the server
ares-server                               # Start with default config
ares-server --config custom.toml          # Use custom config file
ares-server --verbose                     # Enable verbose logging
ares-server --no-color                    # Disable colored output

# Help
ares-server --help                        # Show all options
ares-server init --help                   # Show init options
ares-server --version                     # Show version
```

## 📋 Just Commands (Recommended)

Run `just --list` for all available commands. Here are the most common:

```bash
# Build
just build              # Debug build
just build-release      # Release build
just build-features "x" # Build with features
just build-ui           # Build with embedded UI
just build-full-ui      # Build with all features + UI
just clean              # Clean artifacts

# Run
just run                # Run server
just run-ui             # Run with embedded UI
just run-debug          # Run with debug logging
just run-trace          # Run with trace logging

# CLI Commands
just init               # Initialize project (ares-server init)
just init-openai        # Initialize with OpenAI
just config             # Show configuration
just agents             # List all agents
just agent <name>       # Show agent details

# Test
just test               # Run tests
just test-verbose       # Tests with output
just test-ignored       # Live Ollama tests
just test-all           # All tests
just hurl               # API tests
just hurl-verbose       # API tests (verbose)

# Code Quality
just lint               # Run clippy
just fmt                # Format code
just fmt-check          # Check formatting
just quality            # Format + lint check

# Docker
just docker-up          # Start dev environment
just docker-down        # Stop services
just docker-logs        # View logs
just docker-services    # Start only Ollama + Qdrant

# Ollama
just ollama-status      # Check if running
just ollama-pull        # Pull default model
just ollama-list        # List models

# UI Development
just ui-setup           # Install UI dependencies
just ui-dev             # Run UI dev server
just ui-build           # Build UI for production
just ui-clean           # Clean UI artifacts
just dev                # Run backend + UI together

# Info
just info               # Project info
just status             # Environment status
just --list             # All commands
```

## 🔧 Build Commands (Cargo)

```bash
# Default build (ollama + local-db)
cargo build
# Or: just build

# With specific features
cargo build --features "llamacpp"
cargo build --features "openai"
cargo build --features "llamacpp-cuda"
# Or: just build-features "llamacpp"

# All features
cargo build --features "full"
# Or: just build-all

# With embedded UI
cargo build --features "ui"
# Or: just build-ui

# All features with UI
cargo build --features "full-ui"
# Or: just build-full-ui

# Release build
cargo build --release --features "ollama"
# Or: just build-release

# Minimal build
cargo build --no-default-features
```

## 🧪 Testing

```bash
# Run all tests
cargo test
# Or: just test

# Run specific test file
cargo test --test api_tests
cargo test --test llm_tests
# Or: just test-file api_tests

# Run with features
cargo test --features "llamacpp"

# Run specific test
cargo test test_ollama_client
# Or: just test-filter test_ollama_client

# Show test output
cargo test -- --nocapture
# Or: just test-verbose

# Run ignored tests (live Ollama)
OLLAMA_LIVE_TESTS=1 cargo test -- --ignored
# Or: just test-ignored

# Run all tests
just test-all
```

## 🌐 API Tests (Hurl)

```bash
# Run all hurl tests
just hurl

# Run with verbose output
just hurl-verbose

# Run specific test groups
just hurl-health
just hurl-auth
just hurl-chat
just hurl-research

# Run specific file
just hurl-file hurl/cases/00_health.hurl
```

## 🐳 Docker Commands

```bash
# Start all services (development)
docker compose -f docker-compose.dev.yml up -d
# Or: just docker-up

# Start in foreground
docker compose -f docker-compose.dev.yml up

# Start specific service
docker compose -f docker-compose.dev.yml up ollama

# Start only Ollama + Qdrant (for local development)
just docker-services

# Stop all
docker compose -f docker-compose.dev.yml down
# Or: just docker-down

# View logs
docker compose -f docker-compose.dev.yml logs -f ares
# Or: just docker-logs

# View specific service logs
just docker-logs-service ollama

# Rebuild
docker compose -f docker-compose.dev.yml build --no-cache
# Or: just docker-rebuild
```

## 🤖 Ollama Commands

```bash
# Check if Ollama is running
just ollama-status

# Pull the default model
just ollama-pull

# Pull a specific model
ollama pull ministral-3:3b
ollama pull mistral
# Or: just ollama-pull-model mistral

# List models
ollama list
# Or: just ollama-list

# Run model interactively
ollama run ministral-3:3b

# Delete model
ollama rm ministral-3:3b

# Show model info
ollama show ministral-3:3b
```

## 📦 GGUF Model Setup

```bash
# 1. Create models directory
mkdir -p models

# 2. Download a model (example: Llama 3.2 3B)
cd models
wget https://huggingface.co/bartowski/Llama-3.2-3B-Instruct-GGUF/resolve/main/Llama-3.2-3B-Instruct-Q4_K_M.gguf

# 3. Configure .env
echo "LLAMACPP_MODEL_PATH=./models/Llama-3.2-3B-Instruct-Q4_K_M.gguf" >> .env

# 4. Build and run
cargo run --features llamacpp
```

## � Configuration (TOML + TOON)

A.R.E.S uses a hybrid configuration system:

| Format | Location | Purpose |
|--------|----------|---------|
| **TOML** | `ares.toml` | Infrastructure (server, auth, database, providers) |
| **TOON** | `config/*.toon` | Behavioral (agents, models, tools, workflows) |

### Directory Structure

```
ares/
├── ares.toml                    # Infrastructure config
├── config/
│   ├── agents/                  # Agent definitions (.toon)
│   │   ├── router.toon
│   │   └── orchestrator.toon
│   ├── models/                  # Model configs (.toon)
│   │   ├── fast.toon
│   │   └── balanced.toon
│   ├── tools/                   # Tool configs (.toon)
│   ├── workflows/               # Workflow definitions (.toon)
│   └── mcps/                    # MCP server configs (.toon)
```

### TOON Format Quick Reference

```toon
# Agent example
name: my_agent
model: balanced
tools[2]: calculator,web_search
max_tool_iterations: 10
system_prompt: "Line 1\nLine 2"

# Model example
name: fast
provider: ollama-local
model: llama3.2:1b
temperature: 0.7
max_tokens: 1024
```

**TOON Syntax**:
- Arrays: `tools[2]: a,b` (count prefix)
- Empty arrays: `tools[0]:`
- Newlines in strings: Use `\n` (not YAML `|`)
- Booleans: `true` / `false`

## �🔑 Environment Setup

```bash
# Using just (recommended)
just setup

# Manual: Copy example
cp .env.example .env

# Generate secrets
echo "JWT_SECRET=$(openssl rand -base64 32)" >> .env
echo "API_KEY=$(openssl rand -hex 16)" >> .env

# Edit configuration
nano .env
```

### Essential Variables

```bash
# Server
HOST=127.0.0.1
PORT=3000

# Database
TURSO_URL=file:local.db

# Ollama (default)
OLLAMA_BASE_URL=http://localhost:11434
OLLAMA_MODEL=ministral-3:3b

# LlamaCpp (takes priority)
LLAMACPP_MODEL_PATH=/path/to/model.gguf

# OpenAI
OPENAI_API_KEY=sk-...

# Anthropic
ANTHROPIC_API_KEY=sk-ant-...

# Auth
JWT_SECRET=<32+ chars>
API_KEY=<your-key>
```

## 🔍 Code Quality

```bash
# Format code
cargo fmt
# Or: just fmt

# Check formatting
cargo fmt -- --check
# Or: just fmt-check

# Lint
cargo clippy
# Or: just lint

# Lint with warnings as errors
cargo clippy -- -D warnings

# Lint all features
cargo clippy --all-features
# Or: just lint-all

# Run all quality checks
just quality

# Pre-commit checks (format, lint, test)
just pre-commit

# CI checks
just ci

# Full CI (including live tests)
just ci-full

# Security audit
cargo audit

# Check for updates
cargo outdated
```

## 📊 Diagnostics

```bash
# Check compilation without building
cargo check
# Or: just check

# Check with all features
cargo check --features "full"

# Build documentation
cargo doc --open
# Or: just doc-open

# Generate test coverage
cargo install cargo-llvm-cov
cargo llvm-cov --open

# Project info
just info

# Environment status
just status

# Verify everything works
just verify
```

## 🌐 API Endpoints

### Health Check
```bash
curl http://localhost:3000/health
```

### Register User
```bash
curl -X POST http://localhost:3000/api/auth/register \
  -H "Content-Type: application/json" \
  -d '{"email":"user@example.com","password":"pass123","name":"User"}'
```

### Login
```bash
curl -X POST http://localhost:3000/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"email":"user@example.com","password":"pass123"}'
```

### Chat
```bash
# Get token from login response first
export TOKEN="<your_token>"

curl -X POST http://localhost:3000/api/chat \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"message":"Hello!","agent_type":"general"}'
```

### Swagger UI

> **Note:** Requires the `swagger-ui` feature to be enabled at build time.

```bash
# Build with Swagger UI
cargo build --features "swagger-ui"
# Or use the full bundle:
cargo build --features "full"
```

Then access at: `http://localhost:3000/swagger-ui/`

## 📝 Feature Flags

### LLM Providers
- `ollama` (default) - Local Ollama inference
- `openai` - OpenAI API
- `anthropic` - Anthropic Claude API
- `llamacpp` - Direct GGUF loading
- `llamacpp-cuda` - GGUF with NVIDIA GPU
- `llamacpp-metal` - GGUF with Apple GPU
- `llamacpp-vulkan` - GGUF with Vulkan

### Databases
- `local-db` (default) - SQLite/libsql
- `turso` - Remote Turso database
- `qdrant` - Vector database

### Embeddings
- `local-embeddings` - Local embedding models via ort (ONNX Runtime)

> ⚠️ **Windows MSVC:** The `local-embeddings` feature does NOT work on Windows with the MSVC toolchain due to ort-sys linker errors. Use WSL2 or the GNU toolchain instead.

### UI & Documentation
- `ui` - Embedded Leptos web UI served from backend
- `swagger-ui` - Interactive Swagger UI API documentation at `/swagger-ui/`

> **Note:** `swagger-ui` was made optional in v0.2.5 to reduce binary size and build time. It requires network access during build to download Swagger UI assets.

### Bundles
- `all-llm` - All LLM providers (ollama, openai, anthropic, llamacpp)
- `all-db` - All databases
- `full` - Everything except UI and local-embeddings: ollama, openai, anthropic, llamacpp, turso, qdrant, mcp, swagger-ui
- `full-ui` - Everything including UI (excludes local-embeddings)
- `full-local-embeddings` - Full + local-embeddings (Linux/macOS only)
- `full-ui-local-embeddings` - Full + UI + local-embeddings (Linux/macOS only)
- `minimal` - Nothing optional

> **Note:** `local-embeddings` was removed from `full` and `full-ui` bundles in v0.4.0 due to Windows MSVC compatibility issues. Use `full-local-embeddings` or `full-ui-local-embeddings` on Linux/macOS if you need local embedding support.

## 🐛 Troubleshooting

### Port Already in Use
```bash
# Find process using port 3000
lsof -i :3000          # Linux/Mac
netstat -ano | findstr :3000  # Windows

# Kill process
kill -9 <PID>          # Linux/Mac
taskkill /PID <PID> /F # Windows
```

### Ollama Not Running
```bash
# Check status
curl http://localhost:11434/api/tags

# Start Ollama
ollama serve

# Or via Docker
docker compose -f docker-compose.dev.yml up ollama
```

### Model Not Found (LlamaCpp)
```bash
# Verify file exists
ls -lh models/*.gguf

# Check path in .env
cat .env | grep LLAMACPP_MODEL_PATH

# Test loading
file models/*.gguf  # Should show "GGUF model file"
```

### Compilation Errors
```bash
# Clean build
cargo clean
cargo build

# Update dependencies
cargo update

# Check Rust version (needs 1.91+)
rustc --version
rustup update
```

### Tests Failing
```bash
# Run single test with output
cargo test test_name -- --nocapture

# Update test snapshots (if using insta)
cargo insta review

# Check test isolation
cargo test -- --test-threads=1
```

## 📚 Documentation Links

- **Main README**: `README.md`
- **GGUF Guide**: `docs/GGUF_USAGE.md`
- **Contributing**: `CONTRIBUTING.md`
- **Project Status**: `docs/PROJECT_STATUS.md`
- **API Docs**: Run `cargo doc --open`

## 🎯 Common Workflows

### Add a New Tool
```bash
# 1. Create tool file
touch src/tools/my_tool.rs

# 2. Implement Tool trait
# See src/tools/calculator.rs for example

# 3. Register in mod.rs
echo "pub mod my_tool;" >> src/tools/mod.rs

# 4. Add to registry (src/tools/registry.rs)

# 5. Add tests
touch src/tools/my_tool/tests.rs

# 6. Test
cargo test my_tool
```

### Add a New Agent
```bash
# 1. Create agent file
touch src/agents/my_agent.rs

# 2. Implement Agent trait
# See src/agents/general.rs for example

# 3. Add to AgentType enum
# Edit src/agents/mod.rs

# 4. Register in router
# Edit src/agents/router.rs

# 5. Test
cargo test my_agent
```

### Switch LLM Provider

**To Ollama:**
```bash
# .env
OLLAMA_BASE_URL=http://localhost:11434
OLLAMA_MODEL=ministral-3:3b
# Comment out other providers

cargo run
```

**To LlamaCpp:**
```bash
# .env
LLAMACPP_MODEL_PATH=./models/model.gguf
# LlamaCpp has highest priority

cargo run --features llamacpp
```

**To OpenAI:**
```bash
# .env
OPENAI_API_KEY=sk-...
OPENAI_MODEL=gpt-4
# Comment out LlamaCpp

cargo run --features openai
```

**To Anthropic:**
```bash
# .env
ANTHROPIC_API_KEY=sk-ant-...
ANTHROPIC_MODEL=claude-sonnet-4-20250514

cargo run --features anthropic
```

## 🔐 Security Checklist

- [ ] JWT_SECRET is 32+ characters
- [ ] API_KEY is unique and secure
- [ ] .env is in .gitignore
- [ ] No hardcoded secrets in code
- [ ] HTTPS enabled in production
- [ ] Rate limiting configured
- [ ] Input validation on all endpoints
- [ ] Regular `cargo audit` runs

## 📈 Performance Tips

### CPU Optimization
```bash
# Set threads for LlamaCpp
LLAMACPP_N_THREADS=8  # Match CPU cores

# Reduce context for speed
LLAMACPP_N_CTX=2048
```

### GPU Acceleration
```bash
# NVIDIA CUDA
cargo build --features llamacpp-cuda

# Apple Silicon
cargo build --features llamacpp-metal

# Verify GPU usage
nvidia-smi  # NVIDIA
```

### Model Selection
- Fast: 1B-3B models with Q4_K_M
- Balanced: 7B with Q4_K_M or Q5_K_M
- Quality: 13B+ with Q6_K or Q8_0

## 🎉 Quick Wins

```bash
# Get running in 2 commands (if installed via cargo)
ares-server init
ares-server

# Get running in 3 commands (development)
./scripts/dev-setup.sh
cargo build
cargo run

# Access Swagger UI (requires swagger-ui feature)
cargo build --features swagger-ui
cargo run --features swagger-ui
open http://localhost:3000/swagger-ui/

# With Web UI (if built with --features ui)
cargo build --features ui
cargo run --features ui
open http://localhost:3000/
```

## 💡 Pro Tips

1. **Use `ares-server init`**: Fastest way to get started
2. **Use feature flags wisely**: Don't build with `full` unless needed
3. **Cache Ollama models**: They download once and persist
4. **Use Q4_K_M quantization**: Best quality/size ratio
5. **Monitor RAM usage**: Large models can consume 8GB+
6. **Enable GPU when available**: 5-10x speed boost
7. **Use docker-compose.dev.yml**: Easiest local setup
8. **Check CI before pushing**: Run `cargo clippy` and `cargo test`
9. **Build with `--features ui`**: Get a web interface bundled in
10. **Build with `--features swagger-ui`**: Get interactive API docs at `/swagger-ui/`
11. **Default build is lighter**: Core server doesn't include swagger-ui by default for faster builds

## 🆘 Getting Help

1. Run `ares-server --help` for CLI options
2. Run `ares-server init --help` for init options
3. Check `docs/` directory for guides
4. Search closed issues on GitHub
5. Run `cargo doc --open` for API docs
6. Enable debug logging: `RUST_LOG=debug ares-server`

---

**Quick Links:**
- 📖 [Full Documentation](../README.md)
- 🐛 [Issue Tracker](https://github.com/dirmacs/ares/issues)
- 💬 [Discussions](https://github.com/dirmacs/ares/discussions)
- 🚀 [Latest Release](https://github.com/dirmacs/ares/releases)
