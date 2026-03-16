# A.R.E.S - Justfile
# Command runner for development workflows
# Run `just --list` to see all available commands

# Default recipe - show help
default:
    @just --list

# =============================================================================
# Build Commands
# =============================================================================

# Build the project (debug mode)
build:
    cargo build

# Build the project in release mode
build-release:
    cargo build --release

# Build with specific features
build-features features:
    cargo build --features "{{features}}"

# Build with all features
build-all:
    cargo build --features "full"

# Build with Swagger UI (interactive API docs)
build-swagger:
    cargo build --features "swagger-ui"

# Build with UI feature (auto-detects bun/npm/deno)
build-ui:
    #!/usr/bin/env bash
    set -e
    cd ui
    if command -v bun &> /dev/null; then
        echo "Using bun for UI dependencies..."
        bun install
    elif command -v npm &> /dev/null; then
        echo "Using npm for UI dependencies..."
        npm install
    elif command -v deno &> /dev/null; then
        echo "Using deno for UI dependencies..."
    else
        echo "Error: No Node.js runtime found (bun, npm, or deno)"
        echo "Please install one of these runtimes:"
        echo "  - bun: curl -fsSL https://bun.sh/install | bash"
        echo "  - npm: https://nodejs.org"
        echo "  - deno: https://deno.land"
        exit 1
    fi
    trunk build --release
    cd ..
    cargo build --features "ui"

# Build with all features including UI (auto-detects bun/npm/deno)
build-full-ui:
    #!/usr/bin/env bash
    set -e
    cd ui
    if command -v bun &> /dev/null; then
        echo "Using bun for UI dependencies..."
        bun install
    elif command -v npm &> /dev/null; then
        echo "Using npm for UI dependencies..."
        npm install
    elif command -v deno &> /dev/null; then
        echo "Using deno for UI dependencies..."
    else
        echo "Error: No Node.js runtime found (bun, npm, or deno)"
        exit 1
    fi
    trunk build --release
    cd ..
    cargo build --features "full-ui"

# Clean build artifacts
clean:
    cargo clean

# Check code compiles without building
check:
    cargo check

# =============================================================================
# Run Commands
# =============================================================================

# Run the server (debug mode)
run:
    cargo run

# Run the server in release mode
run-release:
    cargo run --release

# Run with UI embedded
run-ui:
    cargo run --features "ui"

# Run with Swagger UI (interactive API docs at /swagger-ui/)
run-swagger:
    cargo run --features "swagger-ui"

# Run with specific features
run-features features:
    cargo run --features "{{features}}"

# Run with verbose logging
run-debug:
    RUST_LOG=debug cargo run

# Run with trace logging
run-trace:
    RUST_LOG=trace cargo run

# =============================================================================
# Test Commands
# =============================================================================

# Run all tests (non-ignored)
test:
    cargo test

# Run all tests with verbose output
test-verbose:
    cargo test -- --nocapture

# Run ignored tests (requires running services like Ollama)
test-ignored:
    OLLAMA_LIVE_TESTS=1 cargo test -- --ignored

# Run ignored tests with verbose output
test-ignored-verbose:
    OLLAMA_LIVE_TESTS=1 cargo test -- --ignored --nocapture

# Run all tests (including ignored)
test-all:
    cargo test
    OLLAMA_LIVE_TESTS=1 cargo test -- --ignored

# Run all tests with verbose output
test-all-verbose:
    cargo test -- --nocapture
    OLLAMA_LIVE_TESTS=1 cargo test -- --ignored --nocapture

# Run tests matching a pattern
test-filter pattern:
    cargo test {{pattern}} -- --nocapture

# Run a specific test file
test-file file:
    cargo test --test {{file}} -- --nocapture

# Run doc tests
test-doc:
    cargo test --doc

# Run lib tests only
test-lib:
    cargo test --lib

# Run integration tests only
test-integration:
    cargo test --test '*'

# =============================================================================
# Hurl API Tests
# =============================================================================

# Default test configuration
base_url := env_var_or_default("ARES_BASE_URL", "http://127.0.0.1:3000")
test_email := env_var_or_default("ARES_TEST_EMAIL", "hurl.user1@example.com")
test_password := env_var_or_default("ARES_TEST_PASSWORD", "correcthorsebatterystaple")
test_name := env_var_or_default("ARES_TEST_NAME", "Hurl User")

# Common hurl variables
hurl_vars := "--variable base_url=" + base_url + " --variable test_email=" + test_email + " --variable test_password=" + test_password + " --variable test_name=\"" + test_name + "\""

# Run all hurl API tests
hurl:
    @echo "Running Hurl suite against {{base_url}}"
    hurl --test hurl/cases/00_health.hurl {{hurl_vars}}
    hurl --test hurl/cases/01_agents.hurl {{hurl_vars}}
    hurl --test hurl/cases/10_auth_register_login_refresh.hurl {{hurl_vars}}
    hurl --test hurl/cases/11_auth_negative.hurl {{hurl_vars}}
    hurl --test hurl/cases/20_chat_and_memory.hurl {{hurl_vars}}
    hurl --test hurl/cases/21_research.hurl {{hurl_vars}}
    hurl --test hurl/cases/22_protected_negative.hurl {{hurl_vars}}
    @echo "All Hurl cases passed"

# Run all hurl tests with verbose output
hurl-verbose:
    @echo "Running Hurl suite (verbose) against {{base_url}}"
    hurl --very-verbose --test hurl/cases/00_health.hurl {{hurl_vars}}
    hurl --very-verbose --test hurl/cases/01_agents.hurl {{hurl_vars}}
    hurl --very-verbose --test hurl/cases/10_auth_register_login_refresh.hurl {{hurl_vars}}
    hurl --very-verbose --test hurl/cases/11_auth_negative.hurl {{hurl_vars}}
    hurl --very-verbose --test hurl/cases/20_chat_and_memory.hurl {{hurl_vars}}
    hurl --very-verbose --test hurl/cases/21_research.hurl {{hurl_vars}}
    hurl --very-verbose --test hurl/cases/22_protected_negative.hurl {{hurl_vars}}
    @echo "All Hurl cases passed"

# Run a specific hurl test file
hurl-file file:
    hurl --very-verbose --test {{file}} {{hurl_vars}}

# Run hurl health check only
hurl-health:
    hurl --test hurl/cases/00_health.hurl {{hurl_vars}}

# Run hurl auth tests
hurl-auth:
    hurl --test hurl/cases/10_auth_register_login_refresh.hurl {{hurl_vars}}
    hurl --test hurl/cases/11_auth_negative.hurl {{hurl_vars}}

# Run hurl chat tests
hurl-chat:
    hurl --test hurl/cases/20_chat_and_memory.hurl {{hurl_vars}}

# Run hurl research tests
hurl-research:
    hurl --test hurl/cases/21_research.hurl {{hurl_vars}}

# =============================================================================
# Code Quality Commands
# =============================================================================

# Run clippy linter
lint:
    cargo clippy -- -D warnings

# Run clippy with all features (excludes llamacpp on systems without CUDA)
lint-all:
    #!/usr/bin/env bash
    if command -v nvcc &> /dev/null; then
        echo "CUDA detected, running clippy with all features..."
        cargo clippy --all-features -- -D warnings
    else
        echo "CUDA not detected, running clippy without llamacpp feature..."
        cargo clippy --features "ollama,openai,turso,qdrant,mcp" -- -D warnings
    fi

# Run clippy and fix issues automatically
lint-fix:
    cargo clippy --fix --allow-dirty --allow-staged

# Format code
fmt:
    cargo fmt

# Check formatting without changing files
fmt-check:
    cargo fmt -- --check

# Run all code quality checks
quality: fmt-check lint
    @echo "All quality checks passed"

# =============================================================================
# Documentation Commands
# =============================================================================

# Generate documentation
doc:
    cargo doc

# Generate and open documentation
doc-open:
    cargo doc --open

# Generate documentation with private items
doc-private:
    cargo doc --document-private-items

# =============================================================================
# Docker Commands
# =============================================================================

# Build Docker image
docker-build:
    docker build -t ares .

# Build Docker image with features
docker-build-features features:
    docker build --build-arg FEATURES={{features}} -t ares .

# Run with docker-compose (development)
docker-up:
    docker compose -f docker-compose.dev.yml up -d

# Stop docker-compose services
docker-down:
    docker compose -f docker-compose.dev.yml down

# View docker-compose logs
docker-logs:
    docker compose -f docker-compose.dev.yml logs -f

# View specific service logs
docker-logs-service service:
    docker compose -f docker-compose.dev.yml logs -f {{service}}

# Rebuild and restart containers
docker-rebuild:
    docker compose -f docker-compose.dev.yml down
    docker compose -f docker-compose.dev.yml build
    docker compose -f docker-compose.dev.yml up -d

# Start only Ollama and Qdrant (for local development)
docker-services:
    docker compose -f docker-compose.dev.yml up -d ollama qdrant

# =============================================================================
# Database Commands
# =============================================================================

# Reset the local database
db-reset:
    rm -f data/ares.db
    @echo "Database reset complete"

# =============================================================================
# Ollama Commands
# =============================================================================

# Check if Ollama is running
ollama-status:
    @curl -s http://localhost:11434/api/tags > /dev/null && echo "Ollama is running" || echo "Ollama is not running"

# List available Ollama models
ollama-list:
    curl -s http://localhost:11434/api/tags | jq '.models[].name'

# Pull the default model (ministral-3:3b)
ollama-pull:
    ollama pull ministral-3:3b

# Pull a specific model
ollama-pull-model model:
    ollama pull {{model}}

# =============================================================================
# Setup Commands
# =============================================================================

# Initial project setup
setup: setup-env
    @echo "Setup complete! Run 'just run' to start the server."

# Create .env file from template
setup-env:
    #!/usr/bin/env bash
    if [ ! -f .env ]; then
        echo "Creating .env file..."
        cat > .env << 'EOF'
    # A.R.E.S Environment Configuration
    # Generated by: just setup-env

    # Server
    HOST=127.0.0.1
    PORT=3000

    # Database (local SQLite by default)
    TURSO_URL=file:./data/ares.db
    TURSO_AUTH_TOKEN=

    # Ollama Configuration (default provider)
    OLLAMA_BASE_URL=http://localhost:11434
    OLLAMA_MODEL=ministral-3:3b

    # Optional: OpenAI (if you want to use it)
    # OPENAI_API_KEY=sk-...
    # OPENAI_API_BASE=https://api.openai.com/v1
    # OPENAI_MODEL=gpt-4

    # Optional: Qdrant (vector database)
    # QDRANT_URL=http://localhost:6334
    # QDRANT_API_KEY=

    # Authentication
    JWT_SECRET=change-me-in-production-at-least-32-characters-long
    API_KEY=dev-api-key

    # Logging
    RUST_LOG=info,ares=debug
    EOF
        echo ".env file created"
    else
        echo ".env file already exists, skipping"
    fi

# Create data directory
setup-data:
    mkdir -p data models

# Install development dependencies (requires cargo-binstall or manual install)
setup-dev:
    @echo "Installing development tools..."
    cargo install cargo-watch
    cargo install cargo-nextest
    @echo "Development tools installed"

# =============================================================================
# Development Workflow Commands
# =============================================================================

# Watch for changes and rebuild
watch:
    cargo watch -x build

# Watch for changes and run tests
watch-test:
    cargo watch -x test

# Watch for changes and run
watch-run:
    cargo watch -x run

# Full CI check (format, lint, test)
ci: fmt-check lint test
    @echo "CI checks passed"

# Full CI check including ignored tests (requires services)
ci-full: fmt-check lint test-all
    @echo "Full CI checks passed"

# Pre-commit checks
pre-commit: fmt lint test
    @echo "Pre-commit checks passed"

# =============================================================================
# Release Commands
# =============================================================================

# Create a release build
release: clean build-release
    @echo "Release build complete"

# Run benchmarks (if any)
bench:
    cargo bench

# =============================================================================
# Help & Info Commands
# =============================================================================

# Show project info
info:
    @echo "A.R.E.S - Agentic Reasoning and Execution System"
    @echo "================================================"
    @cargo --version
    @rustc --version
    @echo ""
    @echo "Available features:"
    @echo "  - ollama (default): Local LLM inference via Ollama"
    @echo "  - openai: OpenAI API support"
    @echo "  - llamacpp: Direct GGUF model loading"
    @echo "  - turso: Turso/LibSQL database"
    @echo "  - qdrant: Qdrant vector database"
    @echo "  - mcp: Model Context Protocol server"
    @echo "  - swagger-ui: Interactive API docs at /swagger-ui/"
    @echo "  - ui: Embedded Leptos web UI"
    @echo ""
    @echo "Feature bundles:"
    @echo "  - full: All features except UI (includes swagger-ui)"
    @echo "  - full-ui: All features including UI"
    @echo ""
    @echo "Run 'just --list' for available commands"

# Show environment status
status:
    @echo "=== Environment Status ==="
    @echo ""
    @echo "Rust:"
    @cargo --version
    @rustc --version
    @echo ""
    @echo "Services:"
    @just ollama-status || true
    @curl -s http://localhost:6334/healthz > /dev/null 2>&1 && echo "Qdrant is running" || echo "Qdrant is not running"
    @curl -s http://localhost:3000/health > /dev/null 2>&1 && echo "A.R.E.S is running" || echo "A.R.E.S is not running"

# Quick test to verify everything works
verify: build test hurl-health
    @echo "Verification complete - all systems operational"

# =============================================================================
# UI Commands (Leptos Frontend)
# =============================================================================

# Install UI dependencies (run once)
ui-setup:
    cd ui && npm install
    rustup target add wasm32-unknown-unknown
    cargo install trunk --locked || true

# Build the UI for production
ui-build:
    cd ui && npm run build:css
    cd ui && trunk build --release

# Run the UI development server (hot reload)
ui-dev:
    cd ui && trunk serve --open

# Run UI dev server without opening browser
ui-serve:
    cd ui && trunk serve

# Clean UI build artifacts
ui-clean:
    cd ui && rm -rf dist target

# Run both backend and UI dev servers
dev:
    @echo "Starting A.R.E.S backend and UI..."
    @echo "Backend: http://localhost:3000"
    @echo "UI: http://localhost:8080"
    @just run &
    @just ui-dev

# Check for Node.js runtime
check-node:
    #!/usr/bin/env bash
    if command -v bun &> /dev/null; then
        echo "✓ bun $(bun --version)"
    elif command -v npm &> /dev/null; then
        echo "✓ npm $(npm --version)"
    elif command -v deno &> /dev/null; then
        echo "✓ deno $(deno --version | head -1)"
    else
        echo "✗ No Node.js runtime found"
        echo ""
        echo "Install one of the following:"
        echo "  - bun: curl -fsSL https://bun.sh/install | bash"
        echo "  - npm: https://nodejs.org"
        echo "  - deno: https://deno.land"
        exit 1
    fi

# =============================================================================
# CLI Commands
# =============================================================================

# Initialize a new A.R.E.S project in the current directory
init:
    cargo run -- init

# Initialize with OpenAI provider
init-openai:
    cargo run -- init --provider openai

# Initialize with both Ollama and OpenAI
init-both:
    cargo run -- init --provider both

# Show configuration summary
config:
    cargo run -- config

# Validate configuration
config-validate:
    cargo run -- config --validate --full

# List all agents
agents:
    cargo run -- agent list

# Show agent details
agent name:
    cargo run -- agent show {{name}}
