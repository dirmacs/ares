# A.R.E.S Quick Reference Card

Fast reference for common development tasks and commands.

## 🚀 Quick Start

```bash
# 1. Clone and setup
git clone <repo>
cd ares

# 2. Run setup script (auto-configures everything)
./scripts/dev-setup.sh        # Linux/Mac
./scripts/dev-setup.ps1        # Windows

# 3. Build and run
cargo run --features ollama
```

## 🔧 Build Commands

```bash
# Default build (ollama + local-db)
cargo build

# With specific features
cargo build --features "llamacpp"
cargo build --features "openai"
cargo build --features "llamacpp-cuda"

# All features
cargo build --features "full"

# Release build
cargo build --release --features "ollama"

# Minimal build
cargo build --no-default-features
```

## 🧪 Testing

```bash
# Run all tests
cargo test

# Run specific test file
cargo test --test api_tests
cargo test --test llm_tests

# Run with features
cargo test --features "llamacpp"

# Run specific test
cargo test test_ollama_client

# Show test output
cargo test -- --nocapture

# Run ignored tests
cargo test -- --ignored
```

## 🐳 Docker Commands

```bash
# Start all services
docker compose -f docker-compose.dev.yml up

# Start in background
docker compose -f docker-compose.dev.yml up -d

# Start specific service
docker compose -f docker-compose.dev.yml up ollama

# Stop all
docker compose -f docker-compose.dev.yml down

# View logs
docker compose -f docker-compose.dev.yml logs -f ares

# Rebuild
docker compose -f docker-compose.dev.yml build --no-cache
```

## 🤖 Ollama Commands

```bash
# Pull a model
ollama pull llama3.2
ollama pull mistral

# List models
ollama list

# Run model interactively
ollama run llama3.2

# Delete model
ollama rm llama3.2

# Show model info
ollama show llama3.2
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
# Copy example
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
OLLAMA_MODEL=llama3.2

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

# Check formatting
cargo fmt -- --check

# Lint
cargo clippy

# Lint with warnings as errors
cargo clippy -- -D warnings

# Security audit
cargo audit

# Check for updates
cargo outdated
```

## 📊 Diagnostics

```bash
# Check compilation without building
cargo check

# Check with all features
cargo check --features "full"

# Build documentation
cargo doc --open

# Generate test coverage
cargo install cargo-llvm-cov
cargo llvm-cov --open
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

# Check Rust version (needs 1.75+)
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
OLLAMA_MODEL=llama3.2
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