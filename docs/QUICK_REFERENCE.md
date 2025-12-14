# A.R.E.S Quick Reference Card

Fast reference for common development tasks and commands.

## 🚀 Quick Start

```bash
# 1. Clone and setup
git clone <repo>
cd ares

# 2. Install just (command runner)
brew install just          # macOS
# Or: cargo install just

# 3. Run setup (auto-configures everything)
just setup

# 4. Build and run
just run
```

## 📋 Just Commands (Recommended)

Run `just --list` for all available commands. Here are the most common:

```bash
# Build
just build              # Debug build
just build-release      # Release build
just build-features "x" # Build with features
just clean              # Clean artifacts

# Run
just run                # Run server
just run-debug          # Run with debug logging
just run-trace          # Run with trace logging

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
ollama pull granite4:tiny-h
ollama pull mistral
# Or: just ollama-pull-model mistral

# List models
ollama list
# Or: just ollama-list

# Run model interactively
ollama run granite4:tiny-h

# Delete model
ollama rm granite4:tiny-h

# Show model info
ollama show granite4:tiny-h
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

## 🔑 Environment Setup

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
OLLAMA_MODEL=granite4:tiny-h

# LlamaCpp (takes priority)
LLAMACPP_MODEL_PATH=/path/to/model.gguf

# OpenAI
OPENAI_API_KEY=sk-...

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
```
http://localhost:3000/swagger-ui/
```

## 📝 Feature Flags

### LLM Providers
- `ollama` (default) - Local Ollama inference
- `openai` - OpenAI API
- `llamacpp` - Direct GGUF loading
- `llamacpp-cuda` - GGUF with NVIDIA GPU
- `llamacpp-metal` - GGUF with Apple GPU
- `llamacpp-vulkan` - GGUF with Vulkan

### Databases
- `local-db` (default) - SQLite/libsql
- `turso` - Remote Turso database
- `qdrant` - Vector database

### Bundles
- `all-llm` - All LLM providers
- `all-db` - All databases
- `full` - Everything
- `minimal` - Nothing optional

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
OLLAMA_MODEL=granite4:tiny-h
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
# Get running in 3 commands
./scripts/dev-setup.sh
cargo build
cargo run

# Access immediately
open http://localhost:3000/swagger-ui/
```

## 💡 Pro Tips

1. **Use feature flags wisely**: Don't build with `full` unless needed
2. **Cache Ollama models**: They download once and persist
3. **Use Q4_K_M quantization**: Best quality/size ratio
4. **Monitor RAM usage**: Large models can consume 8GB+
5. **Enable GPU when available**: 5-10x speed boost
6. **Use docker-compose.dev.yml**: Easiest local setup
7. **Check CI before pushing**: Run `cargo clippy` and `cargo test`

## 🆘 Getting Help

1. Check `docs/` directory for guides
2. Search closed issues on GitHub
3. Run `cargo doc --open` for API docs
4. Use `--help` flag: `cargo run -- --help`
5. Enable debug logging: `RUST_LOG=debug cargo run`

---

**Quick Links:**
- 📖 [Full Documentation](../README.md)
- 🐛 [Issue Tracker](https://github.com/your-org/ares/issues)
- 💬 [Discussions](https://github.com/your-org/ares/discussions)
- 🚀 [Latest Release](https://github.com/your-org/ares/releases)
